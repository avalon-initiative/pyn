//! `pyn commit` on a git home: one commit with the index as its tree, on the branch `HEAD` is on.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use pyn_git::Ref;
use pyn_gitwork::{Head, Kind, Pathspec};
use pyn_object::{Commit, Kind as ObjectKind, ObjectId, Signature, hash_object};

use super::GitHome;
use super::hooks::author_env;
use super::signing::{SignFlag, sign_commit};
use super::stage::{
    clean_bytes, clean_ident, clear_merge_state, entries_of, env, identity, index_of, merge_heads,
    parse_date, pending_message, refuse_states, stage_working, write_tree,
};

/// The git flags of `pyn commit`; a pyn server takes only `-F` and refuses the rest.
#[derive(clap::Args, Debug, Default)]
pub struct GitCommitArgs {
    /// Read the commit message from a file (`-` is standard input); `--file` does the same on a git repository.
    #[arg(short = 'F', value_name = "FILE")]
    message_file: Option<PathBuf>,
    /// Commit even when nothing changed.
    #[arg(long)]
    allow_empty: bool,
    /// The author, as `Name <email>`.
    #[arg(long, value_name = "AUTHOR")]
    author: Option<String>,
    /// The author date: `<seconds> <+hhmm>`, ISO 8601, RFC 2822 or a day.
    #[arg(long, value_name = "DATE")]
    date: Option<String>,
    /// Add a `Signed-off-by:` line for the committer.
    #[arg(short = 's', long)]
    signoff: bool,
    /// Replace the tip commit with a new one, keeping its message unless `-m` or `-F` gives another.
    #[arg(long)]
    amend: bool,
    /// Open the editor on the message even when `-m`, `-F` or `-C` gave one.
    #[arg(short = 'e', long, overrides_with = "no_edit")]
    edit: bool,
    /// Keep the message and do not open the editor.
    #[arg(long, overrides_with = "edit")]
    no_edit: bool,
    #[command(flatten)]
    sign: SignFlag,
    /// Print nothing on success.
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Accepted for scripts (the message template shows no diff).
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Skip the `pre-commit` (`pre-merge-commit`) and `commit-msg` hooks.
    #[arg(short = 'n', long, overrides_with = "verify")]
    no_verify: bool,
    /// Opposite of `--no-verify`.
    #[arg(long, overrides_with = "no_verify")]
    verify: bool,
    /// Accepted for scripts (there is no message template to add the status to).
    #[arg(long, overrides_with = "no_status")]
    status: bool,
    /// Opposite of `--status`.
    #[arg(long, overrides_with = "status")]
    no_status: bool,
    /// Allow a commit whose message is empty.
    #[arg(long)]
    allow_empty_message: bool,
    /// How the message is cleaned: `strip`, `whitespace`, `verbatim`, `scissors` or `default`.
    #[arg(long, value_name = "MODE")]
    cleanup: Option<String>,
    /// With `--amend` or `-C`, take the author from now on instead of the commit's.
    #[arg(long)]
    reset_author: bool,
    /// Add a trailer, `Token: value` or `Token=value`; repeatable.
    #[arg(long, value_name = "TRAILER")]
    trailer: Vec<String>,
    /// Reuse the message and the author of a commit.
    #[arg(short = 'C', long, value_name = "COMMIT")]
    reuse_message: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Cleanup {
    Strip,
    Whitespace,
    Verbatim,
    Scissors,
}

const CUT_LINE: &[u8] = b"# ------------------------ >8 ------------------------";

impl GitCommitArgs {
    /// The `--cleanup` mode in effect: `default` strips with the editor and only tidies without it.
    fn cleanup_mode(&self, editor: bool) -> Result<Cleanup> {
        match self.cleanup.as_deref() {
            None | Some("default") if editor => Ok(Cleanup::Strip),
            None | Some("default" | "whitespace") => Ok(Cleanup::Whitespace),
            Some("scissors") if editor => Ok(Cleanup::Scissors),
            Some("scissors") => Ok(Cleanup::Whitespace),
            Some("strip") => Ok(Cleanup::Strip),
            Some("verbatim") => Ok(Cleanup::Verbatim),
            Some(other) => bail!("Invalid cleanup mode {other}"),
        }
    }

    /// The first flag that was given, for the refusal on a pyn server.
    pub fn first_given(&self) -> Option<&'static str> {
        let flags = [
            ("--allow-empty", self.allow_empty),
            ("--signoff", self.signoff),
            ("--amend", self.amend),
            ("--edit", self.edit),
            ("--allow-empty-message", self.allow_empty_message),
            ("--cleanup", self.cleanup.is_some()),
            ("--reset-author", self.reset_author),
            ("--trailer", !self.trailer.is_empty()),
            ("--reuse-message", self.reuse_message.is_some()),
        ];
        flags
            .into_iter()
            .find_map(|(f, on)| on.then_some(f))
            .or_else(|| self.sign.first_given())
    }

    /// The first of `--author` and `--date` that was given.
    pub fn author_flag(&self) -> Option<&'static str> {
        [
            ("--author", self.author.is_some()),
            ("--date", self.date.is_some()),
        ]
        .into_iter()
        .find(|(_, on)| *on)
        .map(|(f, _)| f)
    }

    /// The author of a local-store commit: `committer` with `--author` and `--date` applied.
    pub fn server_author(&self, committer: &Signature) -> Result<Signature> {
        let mut author = committer.clone();
        if let Some((name, email)) = self.author.as_deref().map(parse_author).transpose()? {
            author.name = name;
            author.email = email;
        }
        if let Some(d) = &self.date {
            (author.time, author.offset_minutes) = parse_date(d)?;
        }
        Ok(author)
    }

    /// The `-F` message for a commit on a pyn server, cleaned as on a git home; `None` without `-F`.
    pub fn server_message(&self, messages: &[String]) -> Result<Option<String>> {
        let Some(file) = &self.message_file else {
            return Ok(None);
        };
        if !messages.is_empty() {
            bail!("options '-m' and '-F' cannot be used together");
        }
        let raw = read_message(file)?;
        let text = clean_bytes(&raw);
        if text == b"\n" || super::editor::edited_message(&raw).is_none() {
            eprintln!("Aborting commit due to empty commit message.");
            std::process::exit(1);
        }
        let text = String::from_utf8_lossy(&text);
        Ok(Some(text.trim_end_matches('\n').to_string()))
    }
}

#[derive(Default)]
pub struct CommitOptions {
    pub messages: Vec<String>,
    pub all: bool,
    pub untracked: bool,
    pub file: Option<PathBuf>,
    pub flags: GitCommitArgs,
}

fn read_message(path: &PathBuf) -> Result<Vec<u8>> {
    if path.as_os_str() == "-" {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        return Ok(bytes);
    }
    std::fs::read(path).map_err(|e| {
        let why = e.to_string();
        let why = why.split(" (os error").next().unwrap_or(&why);
        anyhow::anyhow!("could not read log file '{}': {why}", path.display())
    })
}

/// `Name <email>` as `--author` takes it.
fn parse_author(text: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let parsed = text.split_once('<').and_then(|(name, rest)| {
        let (email, _) = rest.split_once('>')?;
        Some((clean_ident(name), clean_ident(email)))
    });
    let Some((name, email)) = parsed else {
        bail!(
            "--author '{text}' is not 'Name <email>'; looking up an existing author is not built"
        );
    };
    if name.is_empty() {
        bail!("empty ident name (for <{email}>) not allowed");
    }
    Ok((name.into_bytes(), email.into_bytes()))
}

/// Whether the last paragraph of `lines` is a trailer block, by git's rule: all trailers, or enough of them with
/// a `Signed-off-by:` or cherry-pick line.
fn is_trailer_block(lines: &[&str]) -> bool {
    let (mut trailers, mut others, mut known) = (0, 0, false);
    for line in lines {
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let token = line.split_once(':').map(|(t, _)| t.trim_end());
        let is_trailer = token.is_some_and(|t| {
            !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
        if line.starts_with("Signed-off-by: ") || line.starts_with("(cherry picked from commit ") {
            known = true;
            trailers += 1;
        } else if is_trailer {
            trailers += 1;
        } else {
            others += 1;
        }
    }
    trailers > 0 && (others == 0 || (known && trailers * 3 >= others))
}

/// Appends `line` to a cleaned message: inside the trailer block it ends with, or after a blank line. A message
/// whose last line already is `line` stays as it is.
pub(crate) fn append_trailer(message: Vec<u8>, line: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(&message).into_owned();
    let lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
    if lines.last() == Some(&line) {
        return message;
    }
    let start = lines
        .iter()
        .rposition(|l| l.is_empty())
        .map_or(0, |i| i + 1);
    let mut out = text.trim_end_matches('\n').as_bytes().to_vec();
    if start == 0 || !is_trailer_block(&lines[start..]) {
        out.push(b'\n');
    }
    out.extend_from_slice(format!("\n{line}\n").as_bytes());
    out
}

/// Adds `Signed-off-by:` to a cleaned message unless the last line already is it.
pub(super) fn append_signoff(message: Vec<u8>, name: &[u8], email: &[u8]) -> Vec<u8> {
    let sob = format!(
        "Signed-off-by: {} <{}>",
        String::from_utf8_lossy(name),
        String::from_utf8_lossy(email)
    );
    append_trailer(message, &sob)
}

/// `-m` paragraphs as git joins them: each completed to a line, a blank line between.
fn joined_messages(messages: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for m in messages {
        if !out.is_empty() {
            out.push(b'\n');
        }
        out.extend_from_slice(m.as_bytes());
        if !out.is_empty() && out.last() != Some(&b'\n') {
            out.push(b'\n');
        }
    }
    out
}

/// The message after `--cleanup`; empty when nothing but blanks (or comments, for `strip`) is left.
fn cleaned(raw: &[u8], mode: Cleanup) -> Vec<u8> {
    let out = match mode {
        Cleanup::Verbatim => return raw.to_vec(),
        Cleanup::Whitespace => clean_bytes(raw),
        Cleanup::Scissors => {
            let mut head = Vec::new();
            for line in raw.split_inclusive(|b| *b == b'\n') {
                if line.strip_suffix(b"\n").unwrap_or(line) == CUT_LINE {
                    break;
                }
                head.extend_from_slice(line);
            }
            clean_bytes(&head)
        }
        Cleanup::Strip => {
            let kept: Vec<&[u8]> = raw
                .split(|b| *b == b'\n')
                .filter(|l| !l.starts_with(b"#"))
                .collect();
            clean_bytes(&kept.join(&b'\n'))
        }
    };
    if out == b"\n" { Vec::new() } else { out }
}

/// Where the token ends: a token is alphanumerics and `-`, with blanks allowed only before the separator.
fn trailer_separator(arg: &str) -> Option<usize> {
    let mut blank = false;
    for (i, c) in arg.char_indices() {
        if c == ':' || c == '=' {
            return Some(i);
        }
        if !blank && (c.is_ascii_alphanumeric() || c == '-') {
            continue;
        }
        if i > 0 && (c == ' ' || c == '\t') {
            blank = true;
            continue;
        }
        return None;
    }
    None
}

/// Adds `--trailer` values (`Token: value` or `Token=value`) as git's `interpret-trailers` does.
fn append_trailers(mut message: Vec<u8>, trailers: &[String]) -> Vec<u8> {
    for t in trailers {
        let (token, value) = match trailer_separator(t) {
            Some(i) => (t[..i].trim(), t[i + 1..].trim()),
            None => (t.trim(), ""),
        };
        let line = if value.is_empty() {
            format!("{token}:")
        } else {
            format!("{token}: {value}")
        };
        message = append_trailer(message, &line);
    }
    message
}

fn forced_author(opts: &CommitOptions) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    opts.flags.author.as_deref().map(parse_author).transpose()
}

/// Whether a message holds nothing but `Signed-off-by:` lines, which git counts as no message.
fn only_signoffs(message: &[u8]) -> bool {
    message
        .split(|b| *b == b'\n')
        .all(|l| l.is_empty() || l.starts_with(b"Signed-off-by: "))
}

/// Splits a trailing block of `#` lines (a merge's `# Conflicts:` list) from the message above it.
fn split_comment_block(message: &[u8]) -> (&[u8], &[u8]) {
    let lines: Vec<&[u8]> = message.split_inclusive(|b| *b == b'\n').collect();
    let mut end = lines.len();
    while end > 0 && lines[end - 1].iter().all(u8::is_ascii_whitespace) {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && lines[start - 1].starts_with(b"#") {
        start -= 1;
    }
    if start == end || start == 0 {
        return (message, &[]);
    }
    let at = lines[..start].iter().map(|l| l.len()).sum();
    message.split_at(at)
}

/// Adds the sign-off and the trailers above a trailing comment block, as git does.
fn finish_message(message: Vec<u8>, opts: &CommitOptions, who: &Signature) -> Vec<u8> {
    let (body, comments) = split_comment_block(&message);
    if (!opts.flags.signoff && opts.flags.trailer.is_empty()) || comments.is_empty() {
        return add_trailers(message, opts, who);
    }
    let mut out = add_trailers(body.to_vec(), opts, who);
    out.push(b'\n');
    out.extend_from_slice(comments);
    out
}

fn add_trailers(mut message: Vec<u8>, opts: &CommitOptions, who: &Signature) -> Vec<u8> {
    if opts.flags.signoff {
        message = append_signoff(message, &who.name, &who.email);
    }
    append_trailers(message, &opts.flags.trailer)
}

/// What the `#` lines under an editor message report.
struct Shown<'a> {
    head: &'a Head,
    base: &'a ObjectId,
    has_parent: bool,
    tree: &'a ObjectId,
    index: &'a pyn_git::Index,
    merging: bool,
    /// The `Author:` and `Date:` lines, already formed.
    idents: Vec<String>,
}

fn change_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Added => "new file:  ",
        Kind::Deleted => "deleted:   ",
        Kind::TypeChange => "typechange:",
        _ => "modified:  ",
    }
}

/// The `#` lines under an editor message: the identity, the branch and the staged, unstaged and untracked paths.
fn status_comment(git: &GitHome, shown: &Shown) -> Result<String> {
    let mut out = String::from("#\n");
    if !shown.idents.is_empty() {
        for line in &shown.idents {
            out.push_str(&format!("# {line}\n"));
        }
        out.push_str("#\n");
    }
    match shown.head {
        Head::Branch { name, .. } => out.push_str(&format!("# On branch {name}\n")),
        Head::Detached(id) => {
            out.push_str(&format!(
                "# HEAD detached at {}\n",
                git.repo.abbreviate(id)?
            ));
        }
    }
    if !shown.has_parent {
        out.push_str("#\n# Initial commit\n#\n");
    }
    if shown.merging {
        out.push_str("# All conflicts fixed but you are still merging.\n#\n");
    }
    let staged = git.repo.diff_trees(
        shown.has_parent.then_some(shown.base),
        Some(shown.tree),
        &Pathspec::default(),
    )?;
    if !staged.is_empty() {
        out.push_str("# Changes to be committed:\n");
        for c in staged {
            let what = match (&c.old, &c.new) {
                (None, _) => "new file:  ",
                (_, None) => "deleted:   ",
                _ => "modified:  ",
            };
            out.push_str(&format!("#\t{what} {}\n", super::path_string(&c.path)));
        }
        out.push_str("#\n");
    }
    let unstaged: Vec<_> = git
        .repo
        .unstaged_changes(shown.index)?
        .into_iter()
        .filter(|(_, kind)| *kind != Kind::Unmerged)
        .collect();
    if !unstaged.is_empty() {
        out.push_str("# Changes not staged for commit:\n");
        for (path, kind) in unstaged {
            out.push_str(&format!(
                "#\t{} {}\n",
                change_word(kind),
                super::path_string(&path)
            ));
        }
        out.push_str("#\n");
    }
    let untracked = git.repo.untracked(shown.index)?;
    if !untracked.is_empty() {
        out.push_str("# Untracked files:\n");
        for path in untracked {
            out.push_str(&format!("#\t{}\n", super::path_string(&path)));
        }
        out.push_str("#\n");
    }
    Ok(out)
}

/// The `Author:` (when it is not the committer) and `Date:` (an author date that is not now) lines.
fn author_lines(
    author: &Signature,
    committer: &Signature,
    flags: &GitCommitArgs,
    reused: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    if (author.name.as_slice(), author.email.as_slice())
        != (committer.name.as_slice(), committer.email.as_slice())
    {
        lines.push(format!(
            "Author:    {} <{}>",
            String::from_utf8_lossy(&author.name),
            String::from_utf8_lossy(&author.email)
        ));
    }
    if (reused && !flags.reset_author) || flags.date.is_some() {
        lines.push(format!(
            "Date:      {}",
            crate::time::git_date(author.time, author.offset_minutes)
        ));
    }
    lines
}

/// The editor file: `seed`, the notes for the cleanup mode and a merge, and the `#` status lines.
fn editor_text(seed: &[u8], mode: Cleanup, merging: bool, status: &str) -> Vec<u8> {
    const CUT: &str = "# ------------------------ >8 ------------------------\n# Do not modify or remove the line above.\n# Everything below it will be ignored.\n";
    let mut text = seed.to_vec();
    if !text.is_empty() && !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    let mut tail = String::new();
    if merging {
        if mode == Cleanup::Scissors {
            tail.push_str(CUT);
        }
        tail.push_str(
            "#\n# It looks like you may be committing a merge.\n# If this is not correct, please run\n#\tgit update-ref -d MERGE_HEAD\n# and try again.\n\n",
        );
    }
    tail.push('\n');
    tail.push_str(match mode {
        Cleanup::Strip => {
            "# Please enter the commit message for your changes. Lines starting\n# with '#' will be ignored, and an empty message aborts the commit.\n"
        }
        Cleanup::Scissors if merging => "",
        Cleanup::Scissors => CUT,
        _ => {
            "# Please enter the commit message for your changes. Lines starting\n# with '#' will be kept; you may remove them yourself if you want to.\n# An empty message aborts the commit.\n"
        }
    });
    text.extend_from_slice(tail.as_bytes());
    text.extend_from_slice(status.as_bytes());
    text
}

pub fn commit(git: &GitHome, opts: &CommitOptions) -> Result<()> {
    let rebasing = super::rebase::in_progress(git);
    let blocked: &[&str] = if rebasing {
        &[
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-apply",
            "sequencer",
        ]
    } else {
        &[
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
            "sequencer",
        ]
    };
    refuse_states(git, blocked)?;
    let amend = opts.flags.amend;
    let file = opts.flags.message_file.as_ref().or(opts.file.as_ref());
    if file.is_some() && !opts.messages.is_empty() {
        bail!("options '-m' and '-F' cannot be used together");
    }
    let head = git.repo.head()?;
    let (branch_ref, parent) = match (&head, git.repo.git().refs().head()?) {
        (Head::Branch { id, .. }, Ref::Symbolic(target)) => (Some(target), *id),
        (Head::Detached(id), _) => (None, Some(*id)),
        _ => bail!("HEAD does not name a branch or a commit"),
    };
    let merged = merge_heads(git)?;
    let tip = match (amend, parent) {
        (true, None) => bail!("You have nothing to amend."),
        (true, Some(_)) if !merged.is_empty() => {
            bail!("You are in the middle of a merge -- cannot amend.")
        }
        (true, Some(p)) => Some(git.repo.commit(&p)?),
        _ => None,
    };
    opts.flags.cleanup_mode(true)?;
    let reused = match &opts.flags.reuse_message {
        Some(spec) => {
            if file.is_some() {
                bail!("options '-C' and '-F' cannot be used together");
            }
            if !opts.messages.is_empty() {
                bail!("options '-m' and '-C' cannot be used together");
            }
            let id = git
                .repo
                .resolve(spec)
                .map_err(|_| anyhow::anyhow!("could not lookup commit '{spec}'"))?;
            Some(git.repo.commit(&id)?)
        }
        None => None,
    };
    if opts.flags.reset_author && opts.flags.author.is_some() {
        bail!("options '--reset-author' and '--author' cannot be used together");
    }
    if opts.flags.reset_author && !amend && reused.is_none() {
        bail!("--reset-author can be used only with -C, -c or --amend.");
    }
    let raw = match (file, &reused) {
        (Some(f), _) => Some(read_message(f)?),
        (None, Some(c)) => Some(c.message.clone()),
        (None, None) => (!opts.messages.is_empty()).then(|| joined_messages(&opts.messages)),
    };
    let given = raw.is_some();
    let use_editor = if opts.flags.edit {
        true
    } else {
        !given && !opts.flags.no_edit
    };
    let mode = opts.flags.cleanup_mode(use_editor)?;
    let blank = !use_editor
        && reused.is_none()
        && opts.flags.cleanup.is_none()
        && raw
            .as_ref()
            .is_some_and(|r| super::editor::edited_message(r).is_none());
    let squash_pending = git.repo.git().git_dir().join("SQUASH_MSG").exists();
    let pending = pending_message(git, true, true)?;
    let pending_used = !given && tip.is_none() && (!merged.is_empty() || squash_pending);
    let seed = match (raw, &tip, pending) {
        (Some(r), ..) => r,
        (None, Some(t), _) => t.message.clone(),
        (None, None, Some(m)) if pending_used => m,
        _ => Vec::new(),
    };
    let explicit = if use_editor {
        None
    } else if given || tip.is_some() {
        Some(cleaned(&seed, mode))
    } else {
        pending_used.then(|| seed.clone())
    };
    if (blank || explicit.as_ref().is_some_and(|m| m.is_empty())) && !opts.flags.allow_empty_message
    {
        eprintln!("Aborting commit due to empty commit message.");
        std::process::exit(1);
    }
    if explicit.is_none() && !use_editor && !opts.flags.allow_empty_message {
        eprintln!("Aborting commit due to empty commit message.");
        std::process::exit(1);
    }
    let mut message = explicit.unwrap_or_default();
    let committer = identity(git, "committer")?;
    let seed = match mode {
        Cleanup::Verbatim => seed,
        _ => {
            let tidy = clean_bytes(&seed);
            if tidy == b"\n" { Vec::new() } else { tidy }
        }
    };
    let seed = finish_message(seed, opts, &committer);
    if !use_editor {
        message = finish_message(message, opts, &committer);
    }
    let forced = forced_author(opts)?;
    let kept = if opts.flags.reset_author {
        None
    } else {
        reused
            .as_ref()
            .map(|c| c.author.clone())
            .or_else(|| tip.as_ref().map(|t| t.author.clone()))
    };
    let mut author = match (&kept, &forced) {
        (Some(a), _) => a.clone(),
        (None, Some((name, email))) => {
            let (time, offset_minutes) = match env("GIT_AUTHOR_DATE") {
                Some(d) => parse_date(&d)?,
                None => (
                    chrono::Utc::now().timestamp(),
                    super::stage::local_offset_minutes(),
                ),
            };
            Signature {
                name: name.clone(),
                email: email.clone(),
                time,
                offset_minutes,
            }
        }
        (None, None) => identity(git, "author")?,
    };
    if let (Some(_), Some((name, email))) = (&kept, forced) {
        author.name = name;
        author.email = email;
    }
    if let Some(d) = &opts.flags.date {
        (author.time, author.offset_minutes) = parse_date(d)?;
    }
    let opened = git.open_index()?;
    let mut entries = entries_of(&opened.index);
    let staging = opts.all || opts.untracked;
    let staged_now =
        staging && stage_working(git, &opened.index, &mut entries, (opts.all, opts.untracked))?;
    if entries.keys().any(|(_, stage)| *stage > 0) {
        let mut unmerged: Vec<&Vec<u8>> = entries
            .keys()
            .filter(|(_, stage)| *stage > 0)
            .map(|(path, _)| path)
            .collect();
        unmerged.dedup();
        for path in unmerged {
            println!("U\t{}", super::path_string(path));
        }
        bail!(
            "Committing is not possible because you have unmerged files; resolve them and stage them with `pyn add`"
        );
    }
    let index = index_of(opened.index.version, entries);
    let tree = write_tree(git, &index)?;
    let verify = !opts.flags.no_verify;
    let commit_hook = if merged.is_empty() {
        "pre-commit"
    } else {
        "pre-merge-commit"
    };
    let wants_index = staging
        && [commit_hook, "prepare-commit-msg", "commit-msg"]
            .iter()
            .any(|h| git.hook_present(h));
    let lock = if wants_index {
        Some(IndexLock::create(git, &index)?)
    } else {
        None
    };
    let mut hook_env = author_env(&author);
    hook_env.push(("GIT_EDITOR", ":".to_string()));
    hook_env.push((
        "GIT_INDEX_FILE",
        lock.as_ref()
            .map_or_else(|| git.hook_index(), |l| l.0.display().to_string()),
    ));
    if verify && git.vetoed(commit_hook, &[], &hook_env)? {
        drop(lock);
        std::process::exit(1);
    }
    let empty_tree = hash_object(git.repo.algorithm(), ObjectKind::Tree, b"")?;
    let base = match (&tip, parent) {
        (Some(t), _) => match t.parents.first() {
            Some(p) => git.repo.commit(p)?.tree,
            None => empty_tree,
        },
        (None, Some(p)) => git.repo.commit(&p)?.tree,
        (None, None) => empty_tree,
    };
    let keeps_merge = tip.as_ref().is_some_and(|t| t.parents.len() > 1);
    if tree == base && merged.is_empty() && !opts.flags.allow_empty && !keeps_merge {
        if amend {
            eprintln!(
                "You asked to amend the most recent commit, but doing so would make it empty. \
                 You can repeat your command with --allow-empty."
            );
        } else {
            println!("nothing to commit; stage changes with `pyn add`, or use `pyn commit -a`");
        }
        std::process::exit(1);
    }
    git.warn_if_hooks_off(&[
        "pre-commit",
        "pre-merge-commit",
        "prepare-commit-msg",
        "commit-msg",
        "post-commit",
    ]);
    let source = match (
        &opts.flags.reuse_message,
        given,
        amend,
        merged.is_empty(),
        squash_pending,
    ) {
        (Some(spec), ..) => vec!["commit".to_string(), spec.clone()],
        (None, true, ..) => vec!["message".to_string()],
        (None, false, true, ..) => vec!["commit".to_string(), "HEAD".to_string()],
        (None, false, false, false, _) => vec!["merge".to_string()],
        (None, false, false, true, true) => vec!["squash".to_string()],
        _ => Vec::new(),
    };
    if use_editor {
        let shown = Shown {
            head: &head,
            base: &base,
            has_parent: parent.is_some(),
            tree: &tree,
            index: &index,
            merging: !merged.is_empty(),
            idents: author_lines(&author, &committer, &opts.flags, amend || reused.is_some()),
        };
        let text = editor_text(
            &seed,
            mode,
            !merged.is_empty(),
            &status_comment(git, &shown)?,
        );
        let vetoed = std::cell::Cell::new(false);
        let before = |path: &std::path::Path| {
            let mut args = vec![git.hook_path(path)];
            args.extend(source.iter().cloned());
            if git.vetoed("prepare-commit-msg", &args, &hook_env)? {
                vetoed.set(true);
                bail!("prepare-commit-msg hook failed");
            }
            Ok(())
        };
        match super::editor::edit_text(git, &text, before) {
            Err(_) if vetoed.get() => {
                drop(lock);
                std::process::exit(1);
            }
            Err(e) => return Err(e),
            Ok(edited) => {
                message = cleaned(&edited, mode);
                if mode != Cleanup::Verbatim && only_signoffs(&message) {
                    message.clear();
                }
            }
        }
        if message.is_empty() && !opts.flags.allow_empty_message {
            eprintln!("Aborting commit due to empty commit message.");
            drop(lock);
            std::process::exit(1);
        }
    }
    let prepare = git.hook_present("prepare-commit-msg") && !use_editor;
    if prepare || (verify && git.hook_present("commit-msg")) {
        let edit_msg = git.repo.git().git_dir().join("COMMIT_EDITMSG");
        let shown = git.hook_path(&edit_msg);
        std::fs::write(&edit_msg, &message)?;
        let mut args = vec![shown.clone()];
        args.extend(source);
        let mut vetoed = prepare && git.vetoed("prepare-commit-msg", &args, &hook_env)?;
        if !vetoed && verify {
            vetoed = git.vetoed("commit-msg", &[shown], &hook_env)?;
        }
        if vetoed {
            drop(lock);
            std::process::exit(1);
        }
        message = cleaned(&std::fs::read(&edit_msg)?, mode);
        if message.is_empty() && !opts.flags.allow_empty_message {
            eprintln!("Aborting commit due to empty commit message.");
            drop(lock);
            std::process::exit(1);
        }
    }
    drop(lock);
    let (parents, extra_headers) = match &tip {
        Some(t) => (
            t.parents.clone(),
            t.extra_headers
                .iter()
                .filter(|(k, _)| !k.starts_with(b"gpgsig"))
                .cloned()
                .collect(),
        ),
        None => (
            parent.into_iter().chain(merged.iter().copied()).collect(),
            Vec::new(),
        ),
    };
    let rooted = parents.is_empty();
    let mut record = Commit {
        tree,
        parents,
        author: author.clone(),
        committer: committer.clone(),
        extra_headers,
        message: message.clone(),
    };
    sign_commit(git, &opts.flags.sign, &mut record)?;
    let id: ObjectId = git
        .repo
        .objects()
        .write(ObjectKind::Commit, &record.encode(git.algorithm())?)?;
    if staged_now {
        git.save_index(&opened, &index)?;
    }
    let name = match &head {
        Head::Branch { name, .. } => name.as_str(),
        Head::Detached(_) => "detached HEAD",
    };
    let subject = String::from_utf8_lossy(&message);
    let first = subject.lines().next().unwrap_or_default();
    let kind = match (amend, parent.is_none(), merged.is_empty()) {
        (true, ..) => " (amend)",
        (_, true, _) => " (initial)",
        (_, _, false) => " (merge)",
        _ => "",
    };
    let note = format!("commit{kind}: {first}");
    git.with_ref_env(author_env(&author), || match &branch_ref {
        Some(target) => git.move_ref(target, parent.as_ref(), &id, &note),
        None => git.move_head(&id, &note),
    })
    .context("HEAD moved while committing; run the command again")?;
    clear_merge_state(git);
    super::merge_options::apply_held_autostash(git);
    let root = if rooted { " (root-commit)" } else { "" };
    let changed = git
        .repo
        .diff_trees(
            (base != empty_tree || !rooted).then_some(&base),
            Some(&tree),
            &Pathspec::default(),
        )?
        .len();
    if !opts.flags.quiet {
        println!("[{name}{root} {}] {first}", git.repo.abbreviate(&id)?);
        if (author.name.as_slice(), author.email.as_slice())
            != (committer.name.as_slice(), committer.email.as_slice())
        {
            println!(
                " Author: {} <{}>",
                String::from_utf8_lossy(&author.name),
                String::from_utf8_lossy(&author.email)
            );
        }
        println!("{changed} file(s) changed");
    }
    hook_env.retain(|(k, _)| *k != "GIT_INDEX_FILE");
    hook_env.push(("GIT_INDEX_FILE", git.hook_index()));
    git.run_hook("post-commit", &[], None, &hook_env)?;
    if let (true, Some(old)) = (amend, parent) {
        let pairs = format!("{old} {id}\n");
        let env = author_env(&author);
        git.run_hook(
            "post-rewrite",
            &["amend".to_string()],
            Some(pairs.as_bytes()),
            &env,
        )?;
    }
    Ok(())
}

/// `index.lock` holding the staged index for the hooks of `commit -a`, as git does; removed on drop.
struct IndexLock(PathBuf);

impl IndexLock {
    fn create(git: &GitHome, index: &pyn_git::Index) -> Result<Self> {
        let path = git.repo.git().git_dir().join("index.lock");
        let bytes = index.encode(git.algorithm())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("Unable to create '{}'", path.display()))?;
        std::io::Write::write_all(&mut file, &bytes)?;
        Ok(Self(path))
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sob(message: &str) -> String {
        String::from_utf8(append_signoff(message.as_bytes().to_vec(), b"A", b"a@x")).unwrap()
    }

    #[test]
    fn signoff_joins_a_trailer_block_and_stands_apart_from_prose() {
        assert_eq!(sob("s\n"), "s\n\nSigned-off-by: A <a@x>\n");
        assert_eq!(sob("s\n\nbody\n"), "s\n\nbody\n\nSigned-off-by: A <a@x>\n");
        assert_eq!(
            sob("s\n\nFoo: b\n"),
            "s\n\nFoo: b\nSigned-off-by: A <a@x>\n"
        );
        assert_eq!(
            sob("s\n\ntext\nSigned-off-by: O <o@x>\n"),
            "s\n\ntext\nSigned-off-by: O <o@x>\nSigned-off-by: A <a@x>\n"
        );
        assert_eq!(
            sob("s\n\na\nb\nc\nd\nFoo: b\n"),
            "s\n\na\nb\nc\nd\nFoo: b\n\nSigned-off-by: A <a@x>\n"
        );
    }

    #[test]
    fn signoff_is_not_repeated_on_the_last_line() {
        let done = "s\n\nSigned-off-by: A <a@x>\n";
        assert_eq!(sob(done), done);
    }

    #[test]
    fn authors_need_a_name_and_angle_brackets() {
        assert_eq!(
            parse_author(" N  <n@x>").unwrap(),
            (b"N".to_vec(), b"n@x".to_vec())
        );
        assert!(parse_author("nobody").is_err());
        assert!(parse_author("<n@x>").is_err());
    }
}

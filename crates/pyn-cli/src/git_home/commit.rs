//! `pyn commit` on a git home: one commit with the index as its tree, on the branch `HEAD` is on.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use pyn_git::Ref;
use pyn_gitwork::{Head, Pathspec};
use pyn_object::{Commit, Kind as ObjectKind, ObjectId, Signature, hash_object};

use super::GitHome;
use super::signing::{SignFlag, sign_commit};
use super::stage::{
    clean_bytes, clean_ident, clear_merge_state, entries_of, env, identity, index_of, merge_heads,
    merge_message, parse_date, refuse_states, stage_tracked, warn_about_hooks,
    warn_about_hooks_for, write_tree,
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
    /// Keep the message (accepted for scripts; pyn never opens an editor).
    #[arg(long)]
    no_edit: bool,
    #[command(flatten)]
    sign: SignFlag,
    /// Print nothing on success.
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Accepted for scripts (pyn never opens an editor, so there is no diff to show).
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Accepted for scripts: pyn runs no `pre-commit` or `commit-msg` hook either way.
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
}

impl GitCommitArgs {
    fn cleanup_mode(&self) -> Result<Cleanup> {
        match self.cleanup.as_deref() {
            None | Some("default" | "whitespace" | "scissors") => Ok(Cleanup::Whitespace),
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
        let text = clean_bytes(&read_message(file)?);
        if text == b"\n" {
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
    let mode = opts.flags.cleanup_mode()?;
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
    let explicit = raw.map(|r| cleaned(&r, mode));
    if explicit.as_ref().is_some_and(|m| m.is_empty()) && !opts.flags.allow_empty_message {
        eprintln!("Aborting commit due to empty commit message.");
        std::process::exit(1);
    }
    let mut message = match (explicit, &tip, merge_message(git, opts.flags.no_edit)?) {
        (Some(m), ..) => m,
        (None, Some(t), _) => clean_bytes(&t.message),
        (None, None, Some(m)) if !merged.is_empty() => m,
        _ => crate::usage_error(
            "the following required arguments were not provided:\n  --message <MESSAGE>",
        ),
    };
    let committer = identity(git, "committer")?;
    if opts.flags.signoff {
        message = append_signoff(message, &committer.name, &committer.email);
    }
    message = append_trailers(message, &opts.flags.trailer);
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
    let staged_now = opts.all && stage_tracked(git, &opened.index, &mut entries)?;
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
    if opts.flags.no_verify {
        warn_about_hooks_for(git, &["prepare-commit-msg", "post-commit"]);
    } else {
        warn_about_hooks(git);
    }
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
    match &branch_ref {
        Some(target) => git.move_ref(target, parent.as_ref(), &id, &note),
        None => git.move_head(&id, &note),
    }
    .context("HEAD moved while committing; run the command again")?;
    clear_merge_state(git);
    let root = if rooted { " (root-commit)" } else { "" };
    let changed = git
        .repo
        .diff_trees(
            (base != empty_tree || !rooted).then_some(&base),
            Some(&tree),
            &Pathspec::default(),
        )?
        .len();
    if opts.flags.quiet {
        return Ok(());
    }
    println!("[{name}{root} {}] {first}", &id.to_hex()[..7]);
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
    Ok(())
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

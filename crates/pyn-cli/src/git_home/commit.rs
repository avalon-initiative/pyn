//! `pyn commit` on a git home: one commit with the index as its tree, on the branch `HEAD` is on.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use pyn_git::Ref;
use pyn_gitwork::{Head, Pathspec};
use pyn_object::{Commit, Kind as ObjectKind, ObjectId, Signature, hash_object};

use super::GitHome;
use super::signing::{SignFlag, header_name, header_value, signer_for};
use super::stage::{
    clean_bytes, clean_ident, clean_message, clear_merge_state, entries_of, env, identity,
    index_of, merge_heads, merge_message, parse_date, refuse_states, stage_tracked,
    warn_about_hooks, write_tree,
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
    /// Sign the commit with `user.signingkey` or the given key id.
    #[arg(short = 'S', long, value_name = "KEYID", num_args = 0..=1, require_equals = true, overrides_with = "no_gpg_sign")]
    gpg_sign: Option<Option<String>>,
    /// Do not sign, whatever `commit.gpgsign` says.
    #[arg(long, overrides_with = "gpg_sign")]
    no_gpg_sign: bool,
}

impl GitCommitArgs {
    /// The first flag that was given, for the refusal on a pyn server.
    pub fn first_given(&self) -> Option<&'static str> {
        let flags = [
            ("--allow-empty", self.allow_empty),
            ("--author", self.author.is_some()),
            ("--date", self.date.is_some()),
            ("--signoff", self.signoff),
            ("--amend", self.amend),
            ("--no-edit", self.no_edit),
            ("--gpg-sign", self.gpg_sign.is_some()),
            ("--no-gpg-sign", self.no_gpg_sign),
        ];
        flags.into_iter().find(|(_, on)| *on).map(|(f, _)| f)
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
    let explicit = match file {
        Some(f) => Some(clean_bytes(&read_message(f)?)),
        None => (!opts.messages.is_empty()).then(|| clean_message(&opts.messages)),
    };
    if explicit.as_deref() == Some(b"\n") {
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
    let forced = forced_author(opts)?;
    let mut author = match (&tip, &forced) {
        (Some(t), _) => t.author.clone(),
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
    if let (Some(_), Some((name, email))) = (&tip, forced) {
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
            eprintln!("nothing to commit; stage changes with `pyn add`, or use `pyn commit -a`");
        }
        std::process::exit(1);
    }
    warn_about_hooks(git);
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
    let flag = SignFlag {
        gpg_sign: opts.flags.gpg_sign.clone(),
        no_gpg_sign: opts.flags.no_gpg_sign,
    };
    if let Some(signer) = signer_for(git, &flag, "commit.gpgsign", Some(&committer))? {
        let sig = signer.sign(&record.encode(git.algorithm())?)?;
        record
            .extra_headers
            .push((header_name(git.algorithm()).to_vec(), header_value(sig)));
    }
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

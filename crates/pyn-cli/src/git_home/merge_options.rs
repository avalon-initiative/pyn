//! The options of `merge` that shape its message and its surroundings: `-F`, `--cleanup`, `--log`, `--signoff`,
//! `--into-name`, `--autostash`, `--verify-signatures`, `--quit` and the `merge.*` settings behind them.

use anyhow::{Result, anyhow};
use pyn_gitwork::Pathspec;
use pyn_object::ObjectId;

use super::GitHome;
use super::stage::clean_bytes;
use super::stash::{Applying, PushArgs, apply_commit, save, store_stash};
use crate::commit_format::subject_and_body;
use crate::merging::MergeArgs;

const DEFAULT_LOG: usize = 20;

/// Ends the command with git's `fatal:` line and status 128.
fn fatal(text: &str) -> ! {
    eprintln!("fatal: {text}");
    std::process::exit(128);
}

/// How the message is cleaned: the mode named by `--cleanup`, else what the editor or its absence implies.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Cleanup {
    Strip,
    Whitespace,
    Scissors,
    Verbatim,
}

pub(super) const CUT_LINE: &str = "# ------------------------ >8 ------------------------";
pub(super) const CUT_NOTE: &str =
    "# Do not modify or remove the line above.\n# Everything below it will be ignored.\n";

pub(super) fn cleanup_mode(args: &MergeArgs) -> Cleanup {
    match args.cleanup.as_deref() {
        Some("strip") => Cleanup::Strip,
        Some("verbatim") => Cleanup::Verbatim,
        Some("whitespace") => Cleanup::Whitespace,
        Some("scissors") if args.edit => Cleanup::Scissors,
        Some("scissors") => Cleanup::Whitespace,
        _ if args.edit => Cleanup::Strip,
        _ => Cleanup::Whitespace,
    }
}

/// Refuses what git refuses before the merge starts: a bad `--cleanup`, `--log` or `merge.log`, an unreadable `-F`.
pub(super) fn check_flags(git: &GitHome, args: &MergeArgs) {
    log_limit(git, args);
    if let Some(mode) = args.cleanup.as_deref()
        && !matches!(
            mode,
            "default" | "strip" | "whitespace" | "verbatim" | "scissors"
        )
    {
        fatal(&format!("Invalid cleanup mode {mode}"));
    }
    if args.no_cleanup {
        crate::usage_error("`pyn merge --no-cleanup` is not built");
    }
    if args.no_strategy {
        crate::usage_error("`pyn merge --no-strategy` is not built");
    }
    if args.no_message {
        crate::usage_error("`pyn merge --no-message` is not built");
    }
    if args.file.is_some() && !args.message.is_empty() {
        crate::usage_error("`pyn merge -F` with `-m` is not built");
    }
    if let Some(file) = &args.file
        && std::fs::read(file).is_err()
    {
        crate::usage_error(&format!("could not read file '{file}'"));
    }
}

/// The message `-m` (paragraphs joined by a blank line) or `-F` gave, before cleanup.
pub(super) fn given_message(args: &MergeArgs) -> Result<Option<Vec<u8>>> {
    if let Some(file) = &args.file {
        return std::fs::read(file)
            .map(Some)
            .map_err(|_| anyhow!("could not read file '{file}'"));
    }
    if args.message.is_empty() {
        return Ok(None);
    }
    Ok(Some(args.message.join("\n\n").into_bytes()))
}

/// `text` with its comment lines dropped, then whitespace-cleaned.
pub(super) fn stripped(text: &[u8]) -> Vec<u8> {
    let kept: Vec<&[u8]> = text
        .split(|b| *b == b'\n')
        .filter(|l| !l.starts_with(b"#"))
        .collect();
    clean_bytes(&kept.join(&b'\n'))
}

/// The text before the first scissors line.
fn above_cut_line(text: &[u8]) -> &[u8] {
    let mut start = 0;
    for line in text.split_inclusive(|b| *b == b'\n') {
        if line.strip_suffix(b"\n").unwrap_or(line) == CUT_LINE.as_bytes() {
            return &text[..start];
        }
        start += line.len();
    }
    text
}

/// Applies a cleanup mode to a message that did not go through the editor.
pub(super) fn cleaned(text: Vec<u8>, mode: Cleanup) -> Vec<u8> {
    match mode {
        Cleanup::Verbatim => text,
        Cleanup::Strip => stripped(&text),
        Cleanup::Whitespace => clean_bytes(&text),
        Cleanup::Scissors => clean_bytes(above_cut_line(&text)),
    }
}

/// How many shortlog entries the message gets: `--log[=<n>]`, `--no-log`, then `merge.log` (`true` is 20).
pub(super) fn log_limit(git: &GitHome, args: &MergeArgs) -> usize {
    if args.no_log {
        return 0;
    }
    if let Some(value) = &args.log {
        if value.is_empty() {
            return DEFAULT_LOG;
        }
        match value.parse::<i64>() {
            Ok(n) if n >= 0 => return n as usize,
            Ok(_) => {}
            Err(_) => crate::usage_error("option `log' expects a numerical value"),
        }
    }
    let config = git.config();
    let Some(text) = config.get("merge.log") else {
        return 0;
    };
    match text.parse::<i64>() {
        Ok(n) if n >= 0 => return n as usize,
        Ok(_) => fatal(&format!("error: merge.log: negative length {text}")),
        Err(_) => {}
    }
    match config.get_bool("merge.log") {
        Ok(Some(on)) => usize::from(on) * DEFAULT_LOG,
        _ => fatal(&format!(
            "bad numeric config value '{text}' for 'merge.log': invalid unit"
        )),
    }
}

/// git's shortlog of the commits `theirs` brings in: the non-merge ones, newest first, `limit` of them.
pub(super) fn shortlog(
    git: &GitHome,
    ours: &ObjectId,
    theirs: &ObjectId,
    label: &str,
    limit: usize,
) -> Result<String> {
    let mut walk = git
        .repo
        .log_hiding(&[*theirs], git.reachable(ours)?, Pathspec::default())?;
    let (mut count, mut subjects) = (0, Vec::new());
    while let Some(entry) = walk.next_entry()? {
        if entry.commit.parents.len() > 1 {
            continue;
        }
        count += 1;
        if subjects.len() < limit {
            let message = String::from_utf8_lossy(&entry.commit.message).into_owned();
            subjects.push(subject_and_body(&message).0);
        }
    }
    if count == 0 {
        return Ok(String::new());
    }
    let mut text = if count > limit {
        format!("* {label}: ({count} commits)\n")
    } else {
        format!("* {label}:\n")
    };
    for subject in &subjects {
        text.push_str(&format!("  {subject}\n"));
    }
    if count > limit {
        text.push_str("  ...\n");
    }
    Ok(text)
}

/// The title's destination: `--into-name` or the branch, left out when `merge.suppressDest` (else `main`, `master`) matches.
pub(super) fn destination(git: &GitHome, into_name: Option<&str>, branch: Option<&str>) -> String {
    let dest = into_name.or(branch).unwrap_or("HEAD");
    let config = git.config();
    let patterns = config.get_all("merge.suppressdest");
    let suppressed = if patterns.is_empty() {
        dest == "main" || dest == "master"
    } else {
        patterns
            .iter()
            .any(|p| globset::Glob::new(p).is_ok_and(|g| g.compile_matcher().is_match(dest)))
    };
    if suppressed {
        String::new()
    } else {
        format!(" into {dest}")
    }
}

/// Whether `--ff-only` or `--no-ff` comes from `merge.ff`; the command line wins.
pub(super) fn apply_ff_setting(git: &GitHome, args: &mut MergeArgs) {
    if args.no_ff || args.ff_only || args.ff || args.squash {
        return;
    }
    let config = git.config();
    match config.get("merge.ff") {
        Some(v) if v.eq_ignore_ascii_case("only") => args.ff_only = true,
        Some(_) => {
            if let Ok(Some(false)) = config.get_bool("merge.ff") {
                args.no_ff = true;
            }
        }
        None => {}
    }
}

fn setting(git: &GitHome, name: &str, on: bool, off: bool) -> bool {
    match (on, off) {
        (true, _) => true,
        (_, true) => false,
        _ => git.repo.bool_setting(name, false),
    }
}

pub(super) fn wants_autostash(git: &GitHome, args: &MergeArgs) -> bool {
    setting(git, "merge.autoStash", args.autostash, args.no_autostash)
}

pub(super) fn wants_signature_check(git: &GitHome, args: &MergeArgs) -> bool {
    setting(
        git,
        "merge.verifySignatures",
        args.verify_signatures,
        args.no_verify_signatures,
    )
}

/// Refuses to merge a commit without a good signature, as `--verify-signatures` does.
pub(super) fn verify_signature(git: &GitHome, id: &ObjectId) {
    let checked = git
        .repo
        .objects()
        .read_opt(id)
        .ok()
        .flatten()
        .and_then(|o| super::verify_sig::split_commit(git, &o.data));
    let Some(signed) = checked else {
        fatal(&format!("Commit {id} does not have a GPG signature."));
    };
    match super::verify_sig::check(git, &signed) {
        Ok(result) if result.ok => eprint!("{}", result.output),
        Ok(result) => {
            eprint!("{}", result.output);
            fatal(&format!(
                "Commit {id} has a bad or untrusted GPG signature."
            ));
        }
        Err(e) => fatal(&e.to_string()),
    }
}

fn autostash_file(git: &GitHome) -> std::path::PathBuf {
    git.repo.git().git_dir().join("MERGE_AUTOSTASH")
}

/// Stashes local changes for `--autostash`; the id when there was anything to save.
pub(super) fn create_autostash(git: &GitHome) -> Result<Option<ObjectId>> {
    let args = PushArgs {
        message: Some("autostash".into()),
        ..PushArgs::default()
    };
    let made = save(git, &args, false, true)?.map(|(id, _)| id);
    if let Some(id) = &made {
        eprintln!("Created autostash: {}", super::merge::short(id));
    }
    Ok(made)
}

/// Applies an autostash; when it does not apply cleanly it is kept as the newest stash entry.
pub(super) fn restore_autostash(git: &GitHome, stash: &ObjectId) {
    match apply_commit(git, stash) {
        Ok(Applying::Clean) => eprintln!("Applied autostash."),
        _ => keep_autostash(git, stash, "Applying autostash resulted in conflicts."),
    }
}

fn keep_autostash(git: &GitHome, stash: &ObjectId, why: &str) {
    if store_stash(git, stash, "autostash").is_err() {
        eprintln!("error: could not store the autostash {stash}");
        return;
    }
    eprintln!("{why}");
    eprintln!("Your changes are safe in the stash.");
    eprintln!("You can run \"pyn stash pop\" or \"pyn stash drop\" at any time.");
}

/// Remembers the autostash while the merge waits to be concluded.
pub(super) fn hold_autostash(git: &GitHome, stash: &ObjectId) {
    let _ = std::fs::write(autostash_file(git), format!("{stash}\n"));
    eprintln!("When finished, apply stashed changes with `pyn stash pop`");
}

fn held_autostash(git: &GitHome) -> Option<ObjectId> {
    let text = std::fs::read_to_string(autostash_file(git)).ok()?;
    ObjectId::from_hex(git.repo.algorithm(), text.trim()).ok()
}

/// Applies the autostash a stopped merge held, once the merge is concluded or given up.
pub(super) fn apply_held_autostash(git: &GitHome) {
    if let Some(stash) = held_autostash(git) {
        let _ = std::fs::remove_file(autostash_file(git));
        restore_autostash(git, &stash);
    }
}

/// `merge --quit`: forgets the merge in progress and leaves the index and work tree as they are.
pub(super) fn quit(git: &GitHome) -> Result<()> {
    let dir = git.repo.git().git_dir();
    for name in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    if let Some(stash) = held_autostash(git) {
        let _ = std::fs::remove_file(autostash_file(git));
        keep_autostash(git, &stash, "Autostash exists; creating a new stash entry.");
    }
    Ok(())
}

//! `pyn am` on a git repository: mbox patches applied with `apply --index` and committed with the author, date and
//! message from the email. The session lives in `.git/rebase-apply` with git's file names, so `--continue`,
//! `--skip` and `--abort` work, and git can carry on a session pyn stopped.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::Pathspec;
use pyn_object::{ObjectId, Signature};

use super::GitHome;
use super::am_3way::{self, Outcome};
use super::am_mail::{Mail, parse_mail, split_mbox};
use super::apply::{APPLY_SETTINGS, Options, Session, refuse_settings};
use super::apply_parse::QUIET;
use super::commit::append_signoff;
use super::merge::write_orig_head;
use super::pick::{Pending, make_commit};
use super::plumbing::usage;
use super::signing::SignFlag;
use super::stage::{clean_bytes, identity, parse_date, warn_about_hooks_for, write_tree};
use crate::local_reset::ResetArgs;

const DIR: &str = "rebase-apply";
const HOOKS: [&str; 3] = ["applypatch-msg", "pre-applypatch", "post-applypatch"];

/// Apply patches from mailboxes.
#[derive(Debug, Args)]
pub struct AmArgs {
    /// Carry on after resolving a stopped patch and staging the result.
    #[arg(long = "continue", short = 'r', alias = "resolved")]
    continue_: bool,
    /// Leave the current patch out and carry on with the next.
    #[arg(long)]
    skip: bool,
    /// Stop, restoring the branch to where it was before the session.
    #[arg(long)]
    abort: bool,
    /// Stop, keeping `HEAD` and the index as they are.
    #[arg(long)]
    quit: bool,
    /// Print the patch being applied (`raw`, the whole email, is the default; `diff` is the patch only).
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "raw", value_name = "FORMAT")]
    show_current_patch: Option<String>,
    /// Add a `Signed-off-by` trailer for the committer.
    #[arg(short = 's', long)]
    signoff: bool,
    /// Fall back on a three-way merge when a patch does not apply.
    #[arg(short = '3', long = "3way")]
    three_way: bool,
    /// Do not fall back on a three-way merge.
    #[arg(long)]
    no_3way: bool,
    /// Print less.
    #[arg(short = 'q', long)]
    quiet: bool,
    /// Whitespace errors in added lines: `warn` (the default) or `nowarn`.
    #[arg(long, value_name = "ACTION")]
    whitespace: Option<String>,
    #[command(flatten)]
    sign: SignFlag,
    /// Mailbox files, standard input if none.
    mbox: Vec<String>,
}

struct State {
    dir: PathBuf,
    next: usize,
    last: usize,
    signoff: bool,
    three_way: bool,
    quiet: bool,
    warn_ws: bool,
    sign: SignFlag,
}

fn dir_of(git: &GitHome) -> PathBuf {
    git.repo.git().git_dir().join(DIR)
}

fn put(dir: &Path, name: &str, text: &str) -> Result<()> {
    std::fs::write(dir.join(name), text)?;
    Ok(())
}

fn get(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok()
}

fn flag(dir: &Path, name: &str) -> bool {
    get(dir, name).is_some_and(|t| t.trim() == "t")
}

fn tf(on: bool) -> &'static str {
    if on { "t\n" } else { "f\n" }
}

pub(super) fn am(git: &GitHome, cwd: &Path, args: AmArgs) -> Result<()> {
    refuse_settings(
        git,
        "am",
        &[
            ("am", None),
            APPLY_SETTINGS[0],
            APPLY_SETTINGS[1],
            APPLY_SETTINGS[2],
        ],
    );
    let dir = dir_of(git);
    let in_progress = dir.join("last").exists() && dir.join("next").exists();
    let resuming =
        args.continue_ || args.skip || args.abort || args.quit || args.show_current_patch.is_some();
    if in_progress {
        let mut state = load(&dir)?;
        state.sign = args.sign.clone();
        if args.abort {
            return abort(git, &state);
        }
        if args.quit {
            return destroy(&state.dir);
        }
        if let Some(format) = &args.show_current_patch {
            return show_current(&state, format);
        }
        if args.skip {
            return skip(git, state);
        }
        if args.continue_ {
            return resolve(git, state);
        }
        eprintln!(
            "fatal: previous rebase directory {} still exists but mbox given.",
            dir.display()
        );
        std::process::exit(128);
    }
    if resuming {
        eprintln!("fatal: Resolve operation not in progress, we are not resuming.");
        std::process::exit(128);
    }
    start(git, cwd, args)
}

fn load(dir: &Path) -> Result<State> {
    let number = |name: &str| {
        get(dir, name)
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0)
    };
    Ok(State {
        dir: dir.to_path_buf(),
        next: number("next"),
        last: number("last"),
        signoff: flag(dir, "sign"),
        three_way: flag(dir, "threeway"),
        quiet: flag(dir, "quiet"),
        warn_ws: !get(dir, "apply-opt").is_some_and(|t| t.contains("--whitespace=nowarn")),
        sign: SignFlag::default(),
    })
}

fn destroy(dir: &Path) -> Result<()> {
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

fn head_tree(git: &GitHome) -> Result<Option<ObjectId>> {
    Ok(match git.repo.head_commit()? {
        Some(h) => Some(git.repo.commit(&h)?.tree),
        None => None,
    })
}

/// The paths whose index entry differs from `HEAD`.
fn dirty_paths(git: &GitHome) -> Result<Vec<String>> {
    let opened = git.open_index()?;
    let tree = head_tree(git)?;
    let committed = git.repo.flatten(tree.as_ref(), &Pathspec::default())?;
    let staged = git.repo.index_leaves(&opened.index);
    let mut paths: Vec<String> = committed
        .keys()
        .chain(staged.keys())
        .filter(|p| committed.get(*p) != staged.get(*p))
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    paths.extend(
        opened
            .index
            .entries
            .iter()
            .filter(|e| e.stage > 0)
            .map(|e| String::from_utf8_lossy(&e.path).into_owned()),
    );
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn start(git: &GitHome, _cwd: &Path, args: AmArgs) -> Result<()> {
    if args
        .whitespace
        .as_deref()
        .is_some_and(|w| !matches!(w, "warn" | "nowarn"))
    {
        usage("--whitespace other than warn and nowarn is not supported yet");
    }
    identity(git, "committer")?;
    let mut inputs: Vec<(Vec<u8>, bool)> = Vec::new();
    let from_stdin = |inputs: &mut Vec<(Vec<u8>, bool)>| -> Result<()> {
        if std::io::stdin().is_terminal() {
            usage("am needs a mailbox file or patches on standard input");
        }
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        inputs.push((buf, false));
        Ok(())
    };
    if args.mbox.is_empty() {
        from_stdin(&mut inputs)?;
    }
    for name in &args.mbox {
        if name == "-" {
            from_stdin(&mut inputs)?;
            continue;
        }
        let path = Path::new(name);
        if path.is_dir() {
            usage("am does not read maildirs yet");
        }
        match std::fs::read(path) {
            Ok(buf) => inputs.push((buf, true)),
            Err(e) => {
                eprintln!(
                    "fatal: could not open '{name}' for reading: {}",
                    match e.kind() {
                        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
                        _ => e.to_string(),
                    }
                );
                std::process::exit(128);
            }
        }
    }
    let mut mails: Vec<Vec<u8>> = Vec::new();
    for (input, detect) in inputs
        .iter()
        .filter(|(i, _)| i.iter().any(|b| !b.is_ascii_whitespace()))
    {
        match split_mbox(input, *detect) {
            Ok(found) => mails.extend(found),
            Err(e) => {
                eprintln!("fatal: {e}");
                std::process::exit(128);
            }
        }
    }
    if mails.is_empty() {
        return Ok(());
    }
    let dir = dir_of(git);
    std::fs::create_dir_all(&dir)?;
    for (i, mail) in mails.iter().enumerate() {
        std::fs::write(dir.join(format!("{:04}", i + 1)), mail)?;
    }
    let state = State {
        dir: dir.clone(),
        next: 1,
        last: mails.len(),
        signoff: args.signoff,
        three_way: args.three_way && !args.no_3way,
        quiet: args.quiet,
        warn_ws: args.whitespace.as_deref() != Some("nowarn"),
        sign: args.sign.clone(),
    };
    put(&dir, "applying", "")?;
    put(
        &dir,
        "apply-opt",
        if state.warn_ws {
            ""
        } else {
            " '--whitespace=nowarn'"
        },
    )?;
    put(&dir, "keep", "f\n")?;
    put(&dir, "messageid", "f\n")?;
    put(&dir, "quiet", tf(state.quiet))?;
    put(&dir, "quoted-cr", "")?;
    put(&dir, "scissors", "")?;
    put(&dir, "sign", tf(state.signoff))?;
    put(&dir, "threeway", tf(state.three_way))?;
    put(&dir, "utf8", "t\n")?;
    put(&dir, "last", &format!("{}\n", state.last))?;
    put(&dir, "next", "1\n")?;
    if let Some(head) = git.repo.head_commit()? {
        put(&dir, "abort-safety", &format!("{head}\n"))?;
        write_orig_head(git, &head)?;
    } else {
        put(&dir, "abort-safety", "")?;
        eprintln!("applying to an empty history");
    }
    warn_about_hooks_for(git, &HOOKS);
    let dirty = dirty_paths(git)?;
    if !dirty.is_empty() {
        put(&dir, "dirtyindex", "")?;
        eprintln!(
            "fatal: Dirty index: cannot apply patches (dirty: {})",
            dirty.join(" ")
        );
        std::process::exit(128);
    }
    run(git, state)
}

fn flush() {
    let _ = std::io::stdout().flush();
}

/// The three lines that end every stop.
fn stop() -> ! {
    println!("When you have resolved this problem, run \"pyn am --continue\".");
    println!("If you prefer to skip this patch, run \"pyn am --skip\" instead.");
    println!("To restore the original branch and stop patching, run \"pyn am --abort\".");
    flush();
    std::process::exit(128);
}

fn number(state: &State) -> String {
    format!("{:04}", state.next)
}

/// Reads the mail for the current patch into the state files; false when it has no patch.
fn load_patch(git: &GitHome, state: &State) -> Result<bool> {
    let raw = std::fs::read(state.dir.join(number(state)))?;
    let mail = match parse_mail(&raw) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("fatal: {e}");
            std::process::exit(128);
        }
    };
    let Mail {
        name,
        email,
        date,
        subject,
        body,
        patch,
        ..
    } = mail;
    if email.is_empty() && patch.iter().any(|b| !b.is_ascii_whitespace()) {
        eprintln!("fatal: Patch does not have a valid e-mail address.");
        std::process::exit(128);
    }
    let mut body = clean_bytes(&body);
    let mut subject = subject;
    if subject.is_empty() && !body.is_empty() {
        let end = body.iter().position(|b| *b == b'\n').unwrap_or(body.len());
        subject = String::from_utf8_lossy(&body[..end]).into_owned();
        body = clean_bytes(&body[(end + 1).min(body.len())..]);
    }
    let mut message = subject.clone().into_bytes();
    if !body.is_empty() {
        message.extend_from_slice(b"\n\n");
        message.extend_from_slice(&body);
    }
    let mut message = clean_bytes(&message);
    if state.signoff {
        let me = identity(git, "committer")?;
        message = append_signoff(message, &me.name, &me.email);
    }
    let date_text = date.clone().unwrap_or_default();
    put(
        &state.dir,
        "info",
        &format!("Author: {name}\nEmail: {email}\nSubject: {subject}\nDate: {date_text}\n\n"),
    )?;
    std::fs::write(state.dir.join("final-commit"), &message)?;
    std::fs::write(state.dir.join("msg"), &body)?;
    let quote = |s: &str| s.replace('\'', "'\\''");
    let mut script = format!(
        "GIT_AUTHOR_NAME='{}'\nGIT_AUTHOR_EMAIL='{}'\n",
        quote(&name),
        quote(&email)
    );
    if date.is_some() {
        script.push_str(&format!("GIT_AUTHOR_DATE='{}'\n", quote(&date_text)));
    }
    put(&state.dir, "author-script", &script)?;
    std::fs::write(state.dir.join("patch"), &patch)?;
    Ok(patch.iter().any(|b| !b.is_ascii_whitespace()))
}

fn subject_line(state: &State) -> String {
    get(&state.dir, "final-commit")
        .and_then(|t| t.lines().next().map(str::to_string))
        .unwrap_or_default()
}

fn run(git: &GitHome, mut state: State) -> Result<()> {
    while state.next <= state.last {
        let patch_file = state.dir.join("patch");
        if !patch_file.exists() && !load_patch(git, &state)? {
            println!("Patch is empty.");
            stop();
        }
        let line = subject_line(&state);
        if !state.quiet {
            println!("Applying: {line}");
        }
        flush();
        let patch = std::fs::read(&patch_file)?;
        let applied = apply_patch(git, &patch, state.warn_ws, state.three_way);
        if applied.is_err() {
            if state.three_way {
                match am_3way::fall_back(git, &patch, &line)? {
                    Outcome::Clean => {
                        if index_matches_head(git)? {
                            println!("No changes -- Patch already applied.");
                            next(git, &mut state)?;
                        } else {
                            continue_after(git, &mut state)?;
                        }
                        continue;
                    }
                    Outcome::Conflicted => eprintln!("error: Failed to merge in the changes."),
                    Outcome::Unusable => {}
                }
            }
            eprintln!("hint: Use 'pyn am --show-current-patch=diff' to see the failed patch");
            println!("Patch failed at {} {line}", number(&state));
            stop();
        }
        continue_after(git, &mut state)?;
    }
    destroy(&state.dir)
}

fn continue_after(git: &GitHome, state: &mut State) -> Result<()> {
    commit(git, state)?;
    next(git, state)
}

fn apply_patch(
    git: &GitHome,
    patch: &[u8],
    warn_ws: bool,
    quietly: bool,
) -> std::result::Result<(), i32> {
    let opts = Options {
        strip: 1,
        reverse: false,
        stat: false,
        numstat: false,
        summary: false,
        check: false,
        apply: true,
        index: true,
        cached: false,
        three_way: false,
        ws: warn_ws,
        allow_empty: false,
        prefix: Vec::new(),
    };
    let mut session = Session::new(git, opts).map_err(|e| {
        eprintln!("error: {e:#}");
        128
    })?;
    QUIET.store(quietly, std::sync::atomic::Ordering::Relaxed);
    let done = session
        .run(patch, ".git/rebase-apply/patch")
        .map_err(|f| f.0);
    QUIET.store(false, std::sync::atomic::Ordering::Relaxed);
    done
}

fn index_matches_head(git: &GitHome) -> Result<bool> {
    let opened = git.open_index()?;
    Ok(Some(write_tree(git, &opened.index)?) == head_tree(git)?)
}

/// Removes the files of the finished patch and moves on to the next.
fn next(git: &GitHome, state: &mut State) -> Result<()> {
    let head = git
        .repo
        .head_commit()?
        .map(|h| format!("{h}\n"))
        .unwrap_or_default();
    put(&state.dir, "abort-safety", &head)?;
    for name in ["patch", "info", "msg", "final-commit", "author-script"] {
        let _ = std::fs::remove_file(state.dir.join(name));
    }
    state.next += 1;
    put(&state.dir, "next", &format!("{}\n", state.next))
}

fn author_of(state: &State) -> Result<Signature> {
    let script = get(&state.dir, "author-script").unwrap_or_default();
    let field = |key: &str| -> Option<String> {
        script
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}='")))
            .map(|v| v.strip_suffix('\'').unwrap_or(v).replace("'\\''", "'"))
    };
    let (time, offset) = match field("GIT_AUTHOR_DATE") {
        Some(d) => parse_date(&d)?,
        None => (
            chrono::Utc::now().timestamp(),
            super::stage::local_offset_minutes(),
        ),
    };
    Ok(Signature {
        name: field("GIT_AUTHOR_NAME").unwrap_or_default().into_bytes(),
        email: field("GIT_AUTHOR_EMAIL").unwrap_or_default().into_bytes(),
        time,
        offset_minutes: offset,
    })
}

/// Commits the index as the current patch.
fn commit(git: &GitHome, state: &State) -> Result<()> {
    let opened = git.open_index()?;
    let tree = write_tree(git, &opened.index)?;
    let ours_tree = match head_tree(git)? {
        Some(t) => t,
        None => write_tree(git, &pyn_git::Index::new(2))?,
    };
    let message = std::fs::read(state.dir.join("final-commit"))?;
    make_commit(
        git,
        &Pending {
            tree,
            ours_tree,
            author: author_of(state)?,
            message,
            reflog: "am".to_string(),
            sign: state.sign.clone(),
        },
    )?;
    Ok(())
}

fn resolve(git: &GitHome, mut state: State) -> Result<()> {
    let line = subject_line(&state);
    println!("Applying: {line}");
    let opened = git.open_index()?;
    if !dirty_paths_staged(git, &opened)? {
        println!("No changes - did you forget to use 'pyn add'?");
        println!("If there is nothing left to stage, chances are that something else");
        println!("already introduced the same changes; you might want to skip this patch.");
        stop();
    }
    if opened.index.entries.iter().any(|e| e.stage > 0) {
        println!("You still have unmerged paths in your index.");
        println!("You should 'pyn add' each file with resolved conflicts to mark them as such.");
        println!("You might run `pyn rm` on a file to accept \"deleted by them\" for it.");
        stop();
    }
    continue_after(git, &mut state)?;
    run(git, state)
}

fn dirty_paths_staged(git: &GitHome, opened: &super::stage::OpenIndex) -> Result<bool> {
    let tree = head_tree(git)?;
    let committed = git.repo.flatten(tree.as_ref(), &Pathspec::default())?;
    let staged = git.repo.index_leaves(&opened.index);
    Ok(committed != staged || opened.index.entries.iter().any(|e| e.stage > 0))
}

fn skip(git: &GitHome, mut state: State) -> Result<()> {
    reset_to_head(git)?;
    next(git, &mut state)?;
    run(git, state)
}

fn reset_to_head(git: &GitHome) -> Result<()> {
    let Some(head) = git.repo.head_commit()? else {
        bail!("cannot skip a patch on a branch without commits");
    };
    super::reset::reset(
        git,
        &git.ws.root,
        &ResetArgs::quiet_hard(Some(&head.to_hex())),
    )
}

fn abort(git: &GitHome, state: &State) -> Result<()> {
    if state.dir.join("dirtyindex").exists() {
        return destroy(&state.dir);
    }
    let safe = get(&state.dir, "abort-safety")
        .map(|t| t.trim().to_string())
        .unwrap_or_default();
    let head = git.repo.head_commit()?;
    let orig = git.repo.refs().resolve("ORIG_HEAD")?;
    match (orig, head) {
        (Some(orig), Some(head)) if safe == head.to_hex() => {
            super::reset::reset(
                git,
                &git.ws.root,
                &ResetArgs::quiet_hard(Some(&orig.to_hex())),
            )?;
        }
        _ => {
            eprintln!("warning: You seem to have moved HEAD since the last 'am' failure.");
            eprintln!("Not rewinding to ORIG_HEAD");
        }
    }
    destroy(&state.dir)
}

fn show_current(state: &State, format: &str) -> Result<()> {
    let name = match format {
        "diff" => "patch".to_string(),
        "raw" => number(state),
        other => {
            eprintln!("fatal: invalid value for '--show-current-patch': '{other}'");
            std::process::exit(129);
        }
    };
    let Ok(bytes) = std::fs::read(state.dir.join(&name)) else {
        eprintln!("fatal: Failed to read {}", state.dir.join(&name).display());
        std::process::exit(128);
    };
    super::plumbing::emit_bytes(&bytes);
    Ok(())
}

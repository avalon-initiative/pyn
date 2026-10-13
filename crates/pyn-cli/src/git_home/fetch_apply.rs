//! Storing what a fetch brought: local ref updates, `FETCH_HEAD`, and git's summary lines.

use std::cell::Cell;
use std::io::Write;

use anyhow::Result;
use pyn_git::{Expect, Refspec};
use pyn_object::{Kind, ObjectId};

use super::GitHome;
use super::fetch_map::{Entry, Status, pretty};

const MIN_REFCOL: usize = 10;
const SCREEN_WIDTH: usize = 80;

pub struct Settings {
    /// A clone writes no `FETCH_HEAD` and no reflog lines.
    pub clone: bool,
    pub verbosity: i32,
    pub dry_run: bool,
    pub force: bool,
    pub abbrev: usize,
    /// The reflog action: `fetch` and the arguments as typed.
    pub action: String,
    /// The remote's URL without credentials, `.git` or trailing slashes.
    pub url: String,
    /// The remote's name, for the hint after a ref could not be stored.
    pub remote: Option<String>,
    /// Whether the `From <url>` header is out already.
    pub shown: Cell<bool>,
}

impl Settings {
    /// Prints a summary line, after `From <url>` the first time.
    fn say(&self, text: &str) {
        if self.verbosity < 0 {
            return;
        }
        if !self.shown.replace(true) {
            eprintln!("From {}", self.url);
        }
        eprintln!(" {text}");
    }

    fn summary_width(&self) -> usize {
        2 * self.abbrev + 3
    }

    fn short(&self, id: &ObjectId) -> String {
        id.to_hex()[..self.abbrev].to_owned()
    }
}

/// One output line without its leading space, in git's columns.
fn display(
    s: &Settings,
    code: char,
    summary: &str,
    error: Option<&str>,
    remote: &str,
    local: &str,
    refcol: usize,
) -> String {
    let width = s.summary_width();
    let mut line = format!("{code} {summary:<width$} {remote:<refcol$} -> {local}");
    if let Some(e) = error {
        line.push_str(&format!("  ({e})"));
    }
    line
}

/// A URL as git shows a remote: without the user name, `.git` and trailing slashes.
pub fn tidy_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").map_or(("", url), |(s, r)| (s, r));
    let authority = rest.find('/').unwrap_or(rest.len());
    let anon = match rest[..authority].rfind('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    };
    let url = if scheme.is_empty() {
        anon.to_owned()
    } else {
        format!("{scheme}://{anon}")
    };
    let trimmed = url.trim_end_matches('/');
    match trimmed.strip_suffix(".git") {
        Some(rest) if trimmed.len() > 5 => rest.to_owned(),
        _ => trimmed.to_owned(),
    }
}

/// The `FETCH_HEAD` kind and name for a remote ref name.
fn kind_and_what(name: &str) -> (&'static str, &str) {
    if name == "HEAD" {
        ("", "")
    } else if let Some(rest) = name.strip_prefix("refs/heads/") {
        ("branch", rest)
    } else if let Some(rest) = name.strip_prefix("refs/tags/") {
        ("tag", rest)
    } else if let Some(rest) = name.strip_prefix("refs/remotes/") {
        ("remote-tracking branch", rest)
    } else {
        ("", name)
    }
}

fn fetch_head_line(e: &Entry, url: &str) -> String {
    let (kind, what) = kind_and_what(&e.name);
    let mut note = String::new();
    if !what.is_empty() {
        if !kind.is_empty() {
            note.push_str(kind);
            note.push(' ');
        }
        note.push_str(&format!("'{what}' of "));
    }
    note.push_str(url);
    let marker = if e.status == Status::Merge {
        ""
    } else {
        "not-for-merge"
    };
    format!("{}\t{marker}\t{note}\n", e.id)
}

pub fn truncate_fetch_head(git: &GitHome) -> Result<()> {
    std::fs::write(fetch_head_path(git), b"")?;
    Ok(())
}

fn fetch_head_path(git: &GitHome) -> std::path::PathBuf {
    git.repo.git().git_dir().join("FETCH_HEAD")
}

fn append_fetch_head(git: &GitHome, entries: &[Entry], url: &str) -> Result<()> {
    let lines = |status: fn(Status) -> bool| {
        entries
            .iter()
            .filter(move |e| status(e.status))
            .map(|e| fetch_head_line(e, url))
            .collect::<String>()
    };
    let text = lines(|s| s == Status::Merge) + &lines(|s| s == Status::NotForMerge);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(fetch_head_path(git))?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

/// The commit `id` names, following annotated tags; `None` when it is not a commit or is missing.
fn commit_of(git: &GitHome, id: &ObjectId) -> Option<ObjectId> {
    let mut current = *id;
    for _ in 0..16 {
        let object = git.repo.objects().read_opt(&current).ok()??;
        match object.kind {
            Kind::Commit => return Some(current),
            Kind::Tag => {
                current = pyn_object::Tag::parse(git.repo.algorithm(), &object.data)
                    .ok()?
                    .object;
            }
            _ => return None,
        }
    }
    None
}

/// The width the remote-name column takes: the longest name that fits the screen, at least ten.
fn refcol_width(git: &GitHome, s: &Settings, entries: &[Entry]) -> usize {
    let columns = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(SCREEN_WIDTH);
    let mut width = MIN_REFCOL;
    for e in entries {
        let Some(local) = &e.local else { continue };
        let current = git.repo.refs().resolve(local).ok().flatten();
        if s.verbosity <= 0 && current == Some(e.id) {
            continue;
        }
        let (rlen, llen) = (
            pretty(&e.name).chars().count(),
            pretty(local).chars().count(),
        );
        if 4 + s.summary_width() + rlen + 4 + llen >= columns {
            continue;
        }
        width = width.max(rlen);
    }
    width
}

enum Change {
    FastForward,
    Forced,
    New(&'static str),
    UpdateTag,
}

impl Change {
    fn message(&self) -> &'static str {
        match self {
            Self::FastForward => "fast-forward",
            Self::Forced => "forced-update",
            Self::New(m) => m,
            Self::UpdateTag => "updating tag",
        }
    }
}

/// What a summary line says: its code, summary text and optional reason.
struct Say {
    code: char,
    summary: String,
    error: Option<&'static str>,
}

struct Verdict {
    say: Option<Say>,
    change: Option<Change>,
    rejected: bool,
}

fn verdict(git: &GitHome, s: &Settings, e: &Entry, old: Option<ObjectId>) -> Verdict {
    let local = e
        .local
        .as_deref()
        .expect("a tracking entry has a local ref");
    let say = |code, summary: &str, error| {
        Some(Say {
            code,
            summary: summary.to_owned(),
            error,
        })
    };
    let reject = |why| Verdict {
        say: say('!', "[rejected]", Some(why)),
        change: None,
        rejected: true,
    };
    let change = |c, say| Verdict {
        say,
        change: Some(c),
        rejected: false,
    };
    if old == Some(e.id) {
        return Verdict {
            say: (s.verbosity > 0)
                .then(|| say('=', "[up to date]", None))
                .flatten(),
            change: None,
            rejected: false,
        };
    }
    if old.is_some() && local.starts_with("refs/tags/") {
        if s.force || e.force {
            return change(Change::UpdateTag, say('t', "[tag update]", None));
        }
        return reject("would clobber existing tag");
    }
    let (current, updated) = (old.and_then(|o| commit_of(git, &o)), commit_of(git, &e.id));
    let (Some(current), Some(updated)) = (current, updated) else {
        let (message, what) = if e.name.starts_with("refs/tags/") {
            ("storing tag", "[new tag]")
        } else if e.name.starts_with("refs/heads/") {
            ("storing head", "[new branch]")
        } else {
            ("storing ref", "[new ref]")
        };
        return change(Change::New(message), say('*', what, None));
    };
    let (a, b) = (s.short(&current), s.short(&updated));
    if git.is_ancestor(&current, &updated).unwrap_or(false) {
        change(Change::FastForward, say(' ', &format!("{a}..{b}"), None))
    } else if s.force || e.force {
        change(
            Change::Forced,
            say('+', &format!("{a}...{b}"), Some("forced update")),
        )
    } else {
        reject("non-fast-forward")
    }
}

/// Records `entries` in `FETCH_HEAD` and moves their local refs as git would; true when any could not be updated.
pub fn store(git: &GitHome, s: &Settings, entries: &[Entry]) -> Result<bool> {
    if !s.dry_run && !s.clone {
        append_fetch_head(git, entries, &s.url)?;
    }
    let refcol = refcol_width(git, s, entries);
    let (mut failed, mut blocked) = (false, false);
    for e in entries {
        let Some(local) = &e.local else {
            if e.status != Status::Ignore {
                let (kind, what) = kind_and_what(&e.name);
                let summary = if kind.is_empty() { "branch" } else { kind };
                let what = if what.is_empty() { "HEAD" } else { what };
                s.say(&display(s, '*', summary, None, what, "FETCH_HEAD", refcol));
            }
            continue;
        };
        let refs = git.repo.refs();
        let old = refs.resolve(local).ok().flatten();
        let mut v = verdict(git, s, e, old);
        if let (Some(change), false) = (&v.change, s.dry_run) {
            let expect = old.map_or(Expect::Absent, Expect::Id);
            let message = format!("{}: {}", s.action, change.message());
            let written = refs
                .update_checked(local, expect, &e.id)
                .map_err(anyhow::Error::from)
                .and_then(|()| match s.clone {
                    true => Ok(()),
                    false => git.log_ref(local, old.as_ref(), &e.id, &message, false),
                })
                .and_then(|()| match git.head_target()? {
                    Some(t) if t == *local && !s.clone => {
                        git.log_ref("HEAD", old.as_ref(), &e.id, &message, false)
                    }
                    _ => Ok(()),
                });
            if let Err(err) = written {
                if s.verbosity >= 0 {
                    eprintln!("error: cannot update the ref '{local}': {err}");
                }
                if let Some(say) = &mut v.say {
                    say.code = '!';
                    say.error = Some("unable to update local ref");
                }
                v.rejected = true;
                blocked = true;
            }
        }
        failed |= v.rejected;
        if let Some(Say {
            code,
            summary,
            error,
        }) = v.say
        {
            s.say(&display(
                s,
                code,
                &summary,
                error,
                pretty(&e.name),
                pretty(local),
                refcol,
            ));
        }
    }
    if let (true, true, Some(name)) = (blocked, s.verbosity >= 0, &s.remote) {
        eprintln!(
            "error: some local refs could not be updated; try running\n 'pyn fetch --prune {name}' to remove any old, conflicting branches which may be blocking local refs"
        );
    }
    Ok(failed)
}

/// Local refs a refspec's destination covers whose source the remote no longer has.
pub fn stale_refs(
    git: &GitHome,
    specs: &[Refspec],
    entries: &[Entry],
) -> Result<Vec<(String, ObjectId)>> {
    let refs = git.repo.refs();
    let mut stale = Vec::new();
    for (name, id) in refs.list("refs/")? {
        if matches!(refs.read(&name)?, Some(pyn_git::Ref::Symbolic(_))) {
            continue;
        }
        let source = specs
            .iter()
            .filter(|s| s.dst.as_deref().is_some_and(|d| !d.is_empty()))
            .find_map(|s| s.source_of(&name));
        let Some(source) = source else { continue };
        if !entries
            .iter()
            .any(|e| super::fetch_map::names_match(&source, &e.name))
        {
            stale.push((name, id));
        }
    }
    Ok(stale)
}

/// Deletes the stale refs and their reflogs (or only reports them on a dry run).
pub fn prune(git: &GitHome, s: &Settings, stale: &[(String, ObjectId)]) -> Result<()> {
    let refcol = MIN_REFCOL;
    for (name, id) in stale {
        if !s.dry_run {
            let refs = git.repo.refs();
            refs.delete(name, Expect::Id(*id))?;
            refs.write_reflog(name, &[])?;
        }
        s.say(&display(
            s,
            '-',
            "[deleted]",
            None,
            "(none)",
            pretty(name),
            refcol,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_lose_slashes_and_the_git_suffix() {
        assert_eq!(tidy_url("https://h/x/r.git"), "https://h/x/r");
        assert_eq!(tidy_url("https://h/x/r.git//"), "https://h/x/r");
        assert_eq!(tidy_url("../srv"), "../srv");
        assert_eq!(tidy_url("a.git"), "a.git");
        assert_eq!(tidy_url("ssh://git@h/r.git"), "ssh://h/r");
        assert_eq!(tidy_url("https://u:p@h:8/r/"), "https://h:8/r");
        assert_eq!(tidy_url("git@h:org/r.git"), "h:org/r");
    }

    #[test]
    fn columns_follow_gits_format() {
        let s = Settings {
            clone: false,
            verbosity: 0,
            dry_run: false,
            force: false,
            abbrev: 7,
            action: "fetch".into(),
            url: String::new(),
            remote: None,
            shown: Cell::new(false),
        };
        assert_eq!(
            display(&s, '*', "[new branch]", None, "main", "origin/main", 10),
            "* [new branch]      main       -> origin/main"
        );
        assert_eq!(
            display(
                &s,
                '+',
                "abc1234...def5678",
                Some("forced update"),
                "a",
                "o/a",
                10
            ),
            "+ abc1234...def5678 a          -> o/a  (forced update)"
        );
    }

    #[test]
    fn fetch_head_notes_name_the_kind_and_the_url() {
        let id = ObjectId::from_bytes(pyn_object::Algorithm::Sha1, &[1; 20]).unwrap();
        let mut e = Entry {
            name: "refs/heads/main".into(),
            id,
            local: None,
            force: false,
            status: Status::NotForMerge,
        };
        assert!(fetch_head_line(&e, "u").ends_with("\tnot-for-merge\tbranch 'main' of u\n"));
        e.name = "HEAD".into();
        e.status = Status::Merge;
        assert!(fetch_head_line(&e, "u").ends_with("\t\tu\n"));
        e.name = "refs/notes/x".into();
        assert!(fetch_head_line(&e, "u").ends_with("\t\t'refs/notes/x' of u\n"));
    }
}

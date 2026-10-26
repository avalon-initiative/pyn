//! `pyn bisect` on a git repository: `git bisect`'s binary search, its output and the state it keeps under `.git`
//! (`BISECT_*` files and `refs/bisect/*`), so git and pyn can continue each other's bisection.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use pyn_gitwork::{Head, Pathspec};
use pyn_object::ObjectId;

use super::GitHome;
use super::bisect_bases::merge_bases_many;
use super::bisect_walk::Walk;
use super::commands::add_changes;
use super::plumbing::Resolver;
use super::switch::{Via, checkout_revision};
use crate::commit_format::{Context, Decorations, FormatArgs, Rendering, render, subject_and_body};
use crate::diff::{self, Tree};
use crate::diffstat;

const OK: i32 = 0;
const FAILED: i32 = -1;
const ONLY_SKIPPED_LEFT: i32 = -2;
const MERGE_BASE_CHECK: i32 = -3;
const NO_TESTABLE_COMMIT: i32 = -4;
const FIRST_BAD_FOUND: i32 = -10;
const MERGE_BASE_TESTED: i32 = -11;

const USAGE: &str =
    "usage: pyn bisect start [--term-(bad|new)=<term-new> --term-(good|old)=<term-old>]
                        [--no-checkout] [--first-parent] [<bad> [<good>...]] [--] [<pathspec>...]
   or: pyn bisect (bad|new|<term-new>) [<rev>]
   or: pyn bisect (good|old|<term-old>) [<rev>...]
   or: pyn bisect terms [--term-(good|old) | --term-(bad|new)]
   or: pyn bisect skip [(<rev>|<range>)...]
   or: pyn bisect next
   or: pyn bisect reset [<commit>]
   or: pyn bisect replay <logfile>
   or: pyn bisect log
   or: pyn bisect run <cmd> [<arg>...]
   or: pyn bisect help
";

const HELP: &str = "\
Subcommands (a git repository only; output, state under .git and exit status are git bisect's):
  start [--term-(bad|new)=<t> --term-(good|old)=<t>] [--no-checkout] [--first-parent]
        [<bad> [<good>...]] [--] [<pathspec>...]
  bad|new [<rev>]   good|old [<rev>...]   skip [(<rev>|<range>)...]   next
  reset [<commit>]  log  replay <logfile>  run <cmd> [<arg>...]  terms
Not built (usage error 129): visualize, view.";

const BUILTIN_WORDS: [&str; 11] = [
    "help",
    "start",
    "skip",
    "next",
    "reset",
    "visualize",
    "view",
    "replay",
    "log",
    "run",
    "terms",
];

#[derive(Args, Debug)]
#[command(disable_help_flag = true, after_help = HELP)]
pub struct BisectArgs {
    #[arg(long, action = clap::ArgAction::Help, hide = true)]
    help: Option<bool>,
    /// A subcommand and its arguments, as for `git bisect`.
    #[arg(
        allow_hyphen_values = true,
        trailing_var_arg = true,
        value_name = "ARGS"
    )]
    args: Vec<OsString>,
}

struct Terms {
    bad: String,
    good: String,
}

struct Bisect<'a> {
    git: &'a GitHome,
    cwd: &'a Path,
    terms: Terms,
    /// Output held back so `run` can print it after the step that produced it.
    capture: Option<String>,
    /// What walks have read and marked, kept between walks as git keeps it within a process.
    walk: Walk<'a>,
}

pub(super) fn bisect(git: &GitHome, cwd: &Path, args: BisectArgs) -> Result<()> {
    let argv: Vec<String> = args
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut b = Bisect::new(git, cwd);
    let res = b.dispatch(&argv);
    let _ = std::io::stdout().flush();
    if matches!(res, OK | FIRST_BAD_FOUND | MERGE_BASE_TESTED) {
        return Ok(());
    }
    std::process::exit((-res) & 0xff);
}

fn usage_exit(message: &str) -> ! {
    eprintln!("fatal: {message}\n");
    eprintln!("{USAGE}");
    std::process::exit(129);
}

fn die(message: &str) -> ! {
    eprintln!("fatal: {message}");
    std::process::exit(128);
}

fn error(message: &str) -> i32 {
    eprintln!("error: {message}");
    FAILED
}

/// `'a' 'b'`, each argument preceded by a space, as git's `sq_quote_argv`.
fn sq_quote_argv(args: &[String]) -> String {
    let mut out = String::new();
    for a in args {
        out.push_str(" '");
        for c in a.chars() {
            match c {
                '\'' | '!' => {
                    out.push_str("'\\");
                    out.push(c);
                    out.push('\'');
                }
                _ => out.push(c),
            }
        }
        out.push('\'');
    }
    out
}

/// The words of a `sq_quote_argv` string; `false` as the second value when the quoting is bad.
fn sq_dequote(text: &str) -> (Vec<String>, bool) {
    let b = text.as_bytes();
    let mut out = Vec::new();
    if b.is_empty() {
        return (out, true);
    }
    let mut at = 0;
    loop {
        if b.get(at) != Some(&b'\'') {
            return (out, false);
        }
        let mut word = Vec::new();
        loop {
            at += 1;
            match b.get(at) {
                None => return (out, false),
                Some(b'\'') => {
                    at += 1;
                    if b.get(at) == Some(&b'\\')
                        && matches!(b.get(at + 1), Some(b'\'' | b'!'))
                        && b.get(at + 2) == Some(&b'\'')
                    {
                        word.push(b[at + 1]);
                        at += 2;
                        continue;
                    }
                    break;
                }
                Some(c) => word.push(*c),
            }
        }
        out.push(String::from_utf8_lossy(&word).into_owned());
        match b.get(at) {
            None => return (out, true),
            Some(c) if c.is_ascii_whitespace() => {
                while b.get(at).is_some_and(u8::is_ascii_whitespace) {
                    at += 1;
                }
            }
            Some(_) => return (out, false),
        }
    }
}

fn log2(n: i32) -> i32 {
    31 - n.leading_zeros() as i32
}

fn estimate_steps(all: i32) -> i32 {
    if all < 3 {
        return 0;
    }
    let n = log2(all);
    let e = 1 << n;
    let x = all - e;
    if e < 3 * x { n } else { n - 1 }
}

fn sqrti(val: i32) -> i32 {
    if val == 0 {
        return 0;
    }
    let mut x = val as f32;
    loop {
        let y = (x + val as f32 / x) / 2.0;
        let d = if y > x { y - x } else { x - y };
        x = y;
        if d < 0.5 {
            return x as i32;
        }
    }
}

fn prn(count: i32) -> i32 {
    let c = (count as u32)
        .wrapping_mul(1_103_515_245)
        .wrapping_add(12345);
    ((c / 65536) % 32768) as i32
}

/// Where `skip_away` lands in `count` commits that are not skipped.
fn skip_index(count: i32) -> usize {
    let p = prn(count);
    let at = count.wrapping_mul(p) / 32768 * sqrti(p) / sqrti(32768);
    at.max(0) as usize
}

impl<'a> Bisect<'a> {
    fn new(git: &'a GitHome, cwd: &'a Path) -> Self {
        Self {
            git,
            cwd,
            terms: Terms {
                bad: "bad".into(),
                good: "good".into(),
            },
            capture: None,
            walk: Walk::new(&git.repo),
        }
    }

    fn say(&mut self, text: &str) {
        match &mut self.capture {
            Some(buf) => buf.push_str(text),
            None => {
                let mut out = std::io::stdout().lock();
                if let Err(e) = out.write_all(text.as_bytes()).and_then(|()| out.flush())
                    && e.kind() == std::io::ErrorKind::BrokenPipe
                {
                    std::process::exit(0);
                }
            }
        }
    }

    fn git_dir(&self) -> &Path {
        self.git.repo.git().git_dir()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.git_dir().join(name)
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read(self.path(name))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    fn not_empty(&self, name: &str) -> bool {
        std::fs::metadata(self.path(name)).is_ok_and(|m| m.len() > 0)
    }

    fn write(&self, name: &str, text: &str) -> bool {
        std::fs::write(self.path(name), text).is_ok()
    }

    fn append(&self, name: &str, text: &str) -> bool {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.path(name))
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .is_ok()
    }

    fn unlink(&self, name: &str) {
        let _ = std::fs::remove_file(self.path(name));
    }

    fn pseudo_ref(&self, name: &str) -> Option<ObjectId> {
        let text = self.read(name)?;
        ObjectId::from_hex(self.git.algorithm(), text.split_whitespace().next()?).ok()
    }

    fn set_pseudo_ref(&self, name: &str, id: &ObjectId) -> Result<()> {
        let path = self.path(name);
        let lock = self.path(&format!("{name}.lock"));
        std::fs::write(&lock, format!("{id}\n"))?;
        std::fs::rename(&lock, &path)?;
        Ok(())
    }

    fn bisect_refs(&self) -> Vec<(String, ObjectId)> {
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let name = format!("{prefix}{}", e.file_name().to_string_lossy());
                if e.path().is_dir() {
                    walk(&e.path(), &format!("{name}/"), out);
                } else if !name.ends_with(".lock") {
                    out.push(name);
                }
            }
        }
        let mut names = Vec::new();
        walk(&self.path("refs/bisect"), "", &mut names);
        names.sort();
        names
            .into_iter()
            .filter_map(|n| {
                let id = self
                    .git
                    .repo
                    .refs()
                    .resolve(&format!("refs/bisect/{n}"))
                    .ok()??;
                Some((n, id))
            })
            .collect()
    }

    fn bisect_ref(&self, name: &str) -> Option<ObjectId> {
        self.git
            .repo
            .refs()
            .resolve(&format!("refs/bisect/{name}"))
            .ok()
            .flatten()
    }

    fn commit_of(&self, spec: &str) -> Option<ObjectId> {
        let id = self.any_object(spec)?;
        super::plumbing::peel(self.git, &id, Some(pyn_object::Kind::Commit))
            .ok()
            .flatten()
    }

    fn any_object(&self, spec: &str) -> Option<ObjectId> {
        let resolver = Resolver {
            git: self.git,
            prefix: &[],
        };
        resolver
            .rev(spec)
            .ok()
            .flatten()
            .or_else(|| self.git.repo.resolve(spec).ok())
    }

    fn subject(&self, id: &ObjectId) -> String {
        let message = self
            .git
            .repo
            .commit(id)
            .map(|c| String::from_utf8_lossy(&c.message).into_owned())
            .unwrap_or_default();
        subject_and_body(&message).0
    }

    fn checkout(&self, target: &str, quiet: bool, any_worktree: bool) -> bool {
        let via = Via {
            quiet,
            any_worktree,
            returned: true,
        };
        match checkout_revision(self.git, target, via) {
            Ok(()) => true,
            Err(e) => {
                if !e.to_string().is_empty() {
                    eprintln!("fatal: {e}");
                }
                false
            }
        }
    }

    fn dispatch(&mut self, argv: &[String]) -> i32 {
        let Some(first) = argv.first() else {
            usage_exit("need a command");
        };
        if first == "-h" {
            println!("{USAGE}");
            std::process::exit(129);
        }
        if first.starts_with('-') {
            let name = first.trim_start_matches('-');
            eprintln!("error: unknown option `{name}'");
            eprintln!("{USAGE}");
            std::process::exit(129);
        }
        let rest = &argv[1..];
        match first.as_str() {
            "reset" => {
                if rest.len() > 1 {
                    return error("'pyn bisect reset' requires either no argument or a commit");
                }
                self.reset(rest.first().map(String::as_str))
            }
            "terms" => {
                if rest.len() > 1 {
                    return error("'pyn bisect terms' requires 0 or 1 argument");
                }
                self.terms_command(rest.first().map(String::as_str))
            }
            "start" => self.start(rest),
            "next" => {
                if !rest.is_empty() {
                    return error("'pyn bisect next' requires 0 arguments");
                }
                let known = self.get_terms();
                self.next(known)
            }
            "log" => self.log(),
            "replay" => {
                if rest.len() != 1 {
                    return error("no logfile given");
                }
                self.replay(&rest[0])
            }
            "skip" => {
                self.get_terms();
                self.skip(rest)
            }
            "visualize" | "view" => {
                eprintln!("error: `pyn bisect {first}` is not available");
                std::process::exit(129);
            }
            "run" => {
                if rest.is_empty() {
                    return error("'pyn bisect run' failed: no command provided.");
                }
                self.get_terms();
                self.run(rest)
            }
            "help" => {
                eprintln!("{USAGE}");
                std::process::exit(129);
            }
            word => {
                self.get_terms();
                if self.check_and_set_terms(word) != OK
                    || (word != self.terms.good && word != self.terms.bad)
                {
                    usage_exit(&format!("unknown command: '{word}'"));
                }
                self.state(argv)
            }
        }
    }

    fn get_terms(&mut self) -> bool {
        let Some(text) = self.read("BISECT_TERMS") else {
            return false;
        };
        let mut lines = text.split('\n');
        self.terms.bad = lines.next().unwrap_or_default().to_string();
        self.terms.good = lines.next().unwrap_or_default().to_string();
        true
    }

    fn check_term_format(&self, term: &str, orig: &str) -> i32 {
        if pyn_git::check_ref_name(&format!("refs/bisect/{term}")).is_err() {
            return error(&format!("'{term}' is not a valid term"));
        }
        if BUILTIN_WORDS.contains(&term) {
            return error(&format!("can't use the builtin command '{term}' as a term"));
        }
        if (orig != "bad" && matches!(term, "bad" | "new"))
            || (orig != "good" && matches!(term, "good" | "old"))
        {
            return error(&format!("can't change the meaning of the term '{term}'"));
        }
        OK
    }

    fn write_terms(&self, bad: &str, good: &str) -> i32 {
        if bad == good {
            return error("please use two different terms");
        }
        if self.check_term_format(bad, "bad") != OK || self.check_term_format(good, "good") != OK {
            return FAILED;
        }
        if self.write("BISECT_TERMS", &format!("{bad}\n{good}\n")) {
            OK
        } else {
            FAILED
        }
    }

    fn check_and_set_terms(&mut self, cmd: &str) -> i32 {
        let has_file = self.not_empty("BISECT_TERMS");
        if matches!(cmd, "skip" | "start" | "terms") {
            return OK;
        }
        if has_file && cmd != self.terms.bad && cmd != self.terms.good {
            return error(&format!(
                "Invalid command: you're currently in a {}/{} bisect",
                self.terms.bad, self.terms.good
            ));
        }
        if !has_file {
            let pair = match cmd {
                "bad" | "good" => Some(("bad", "good")),
                "new" | "old" => Some(("new", "old")),
                _ => None,
            };
            if let Some((bad, good)) = pair {
                self.terms = Terms {
                    bad: bad.into(),
                    good: good.into(),
                };
                return self.write_terms(bad, good);
            }
        }
        OK
    }

    fn terms_command(&mut self, option: Option<&str>) -> i32 {
        if !self.get_terms() {
            return error("no terms defined");
        }
        match option {
            None => self.say(&format!(
                "Your current terms are '{}' for the old state\nand '{}' for the new state.\n",
                self.terms.good, self.terms.bad
            )),
            Some("--term-good" | "--term-old") => {
                let t = format!("{}\n", self.terms.good);
                self.say(&t);
            }
            Some("--term-bad" | "--term-new") => {
                let t = format!("{}\n", self.terms.bad);
                self.say(&t);
            }
            Some(o) => {
                return error(&format!(
                    "invalid argument {o} for 'pyn bisect terms'.\nSupported options are: --term-good|--term-old and --term-bad|--term-new."
                ));
            }
        }
        OK
    }

    fn clean_state(&self) -> i32 {
        for (name, _) in self.bisect_refs() {
            let _ = self
                .git
                .repo
                .refs()
                .delete(&format!("refs/bisect/{name}"), pyn_git::Expect::Any);
        }
        let _ = std::fs::remove_dir(self.path("refs/bisect"));
        for name in [
            "BISECT_HEAD",
            "BISECT_EXPECTED_REV",
            "BISECT_ANCESTORS_OK",
            "BISECT_LOG",
            "BISECT_NAMES",
            "BISECT_RUN",
            "BISECT_TERMS",
            "BISECT_FIRST_PARENT",
            "BISECT_START",
        ] {
            self.unlink(name);
        }
        OK
    }

    fn reset(&mut self, commit: Option<&str>) -> i32 {
        let branch = match commit {
            None => match self.read("BISECT_START") {
                Some(text) if text.is_empty() => {
                    self.say("We are not bisecting.\n");
                    String::new()
                }
                Some(text) => text.trim_end().to_string(),
                None => String::new(),
            },
            Some(c) => {
                if self.commit_of(c).is_none() {
                    return error(&format!("'{c}' is not a valid commit"));
                }
                c.to_string()
            }
        };
        if !branch.is_empty()
            && self.pseudo_ref("BISECT_HEAD").is_none()
            && !self.checkout(&branch, false, true)
        {
            return error(&format!(
                "could not check out original HEAD '{branch}'. Try 'pyn bisect reset <commit>'."
            ));
        }
        self.clean_state()
    }

    fn log_commit(&self, state: &str, id: &ObjectId) {
        let line = format!("# {state}: [{id}] {}\n", self.subject(id));
        self.append("BISECT_LOG", &line);
    }

    fn write_state(&self, state: &str, rev: &str, nolog: bool) -> i32 {
        let tag = if state == self.terms.bad {
            state.to_string()
        } else if state == self.terms.good || state == "skip" {
            format!("{state}-{rev}")
        } else {
            return error(&format!("Bad bisect_write argument: {state}"));
        };
        let Some(id) = self.any_object(rev) else {
            return error(&format!("couldn't get the oid of the rev '{rev}'"));
        };
        if let Err(e) = self
            .git
            .repo
            .refs()
            .update(&format!("refs/bisect/{tag}"), &id)
        {
            eprintln!("error: {e}");
            return FAILED;
        }
        self.log_commit(state, &id);
        if !nolog {
            self.append("BISECT_LOG", &format!("git bisect {state} {rev}\n"));
        }
        OK
    }

    fn prompt(&self, text: &str) -> String {
        eprint!("{text}");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        line.trim().to_string()
    }

    fn counts(&self) -> (usize, bool) {
        let good = format!("{}-", self.terms.good);
        let refs = self.bisect_refs();
        let nr_good = refs.iter().filter(|(n, _)| n.starts_with(&good)).count();
        (nr_good, refs.iter().any(|(n, _)| *n == self.terms.bad))
    }

    fn next_check(&mut self, current: Option<&str>) -> i32 {
        let (nr_good, have_bad) = self.counts();
        let (missing_good, missing_bad) = (nr_good == 0, !have_bad);
        if !missing_good && !missing_bad {
            return OK;
        }
        let Some(current) = current else {
            return FAILED;
        };
        if missing_good && !missing_bad && current == self.terms.good {
            eprintln!("warning: bisecting only with a {} commit", self.terms.bad);
            if !std::io::stdin().is_terminal() {
                return OK;
            }
            let answer = self.prompt("Are you sure [Y/n]? ");
            return if answer.starts_with(['n', 'N']) {
                FAILED
            } else {
                OK
            };
        }
        let (bad, good) = ("bad|new", "good|old");
        if self.not_empty("BISECT_START") {
            error(&format!(
                "You need to give me at least one {bad} and {good} revision.\nYou can use \"pyn bisect {bad}\" and \"pyn bisect {good}\" for that."
            ))
        } else {
            error(&format!(
                "You need to start by \"pyn bisect start\".\nYou then need to give me at least one {good} and {bad} revision.\nYou can use \"pyn bisect {good}\" and \"pyn bisect {bad}\" for that."
            ))
        }
    }

    fn log_say(&mut self, text: &str) {
        self.say(text);
        self.append("BISECT_LOG", &format!("# {text}"));
    }

    fn print_status(&mut self) {
        let (nr_good, have_bad) = self.counts();
        if nr_good > 0 && have_bad {
            return;
        }
        let (good, bad) = (self.terms.good.clone(), self.terms.bad.clone());
        let text = if nr_good == 0 && !have_bad {
            format!("status: waiting for both '{good}' and '{bad}' commits\n")
        } else if nr_good > 0 {
            let s = if nr_good == 1 { "" } else { "s" };
            format!("status: waiting for '{bad}' commit, {nr_good} '{good}' commit{s} known\n")
        } else {
            format!("status: waiting for '{good}' commit(s), '{bad}' commit known\n")
        };
        self.log_say(&text);
    }

    fn autostart(&mut self) -> i32 {
        if self.not_empty("BISECT_START") {
            return OK;
        }
        eprintln!("You need to start by \"pyn bisect start\"\n");
        if !std::io::stdin().is_terminal() {
            return FAILED;
        }
        let answer = self.prompt("Do you want me to do it for you [Y/n]? ");
        if answer.starts_with(['n', 'N']) {
            FAILED
        } else {
            self.start(&[])
        }
    }

    fn auto_next(&mut self) -> i32 {
        if self.next_check(None) != OK {
            self.print_status();
            return OK;
        }
        self.next(true)
    }

    fn next(&mut self, terms_known: bool) -> i32 {
        if self.autostart() != OK {
            return FAILED;
        }
        let current = terms_known.then(|| self.terms.good.clone());
        if self.next_check(current.as_deref()) != OK {
            return FAILED;
        }
        let res = self.next_all();
        if res == FIRST_BAD_FOUND {
            let res = self.successful();
            return if res != OK { res } else { FIRST_BAD_FOUND };
        }
        if res == ONLY_SKIPPED_LEFT {
            let res = self.skipped_commits();
            return if res != OK { res } else { ONLY_SKIPPED_LEFT };
        }
        res
    }

    fn successful(&mut self) -> i32 {
        let Some(bad) = self.bisect_ref(&self.terms.bad) else {
            return FAILED;
        };
        let line = format!(
            "# first '{}' commit: [{bad}] {}\n",
            self.terms.bad,
            self.subject(&bad)
        );
        if self.append("BISECT_LOG", &line) {
            OK
        } else {
            FAILED
        }
    }

    fn skipped_commits(&mut self) -> i32 {
        let refs = self.bisect_refs();
        let good = format!("{}-", self.terms.good);
        let Some((_, bad)) = refs.iter().find(|(n, _)| n.starts_with(&self.terms.bad)) else {
            return FAILED;
        };
        let goods: Vec<ObjectId> = refs
            .iter()
            .filter(|(n, _)| n.starts_with(&good))
            .map(|(_, id)| *id)
            .collect();
        self.walk.reset_walk();
        self.walk.configure(Pathspec::default(), false);
        let mut args = vec![(*bad, false)];
        args.extend(goods.iter().map(|g| (*g, true)));
        let Ok(popped) = self.walk.limit(&args) else {
            return error("revision walk setup failed");
        };
        let mut text = String::from("# only skipped commits left to test\n");
        for id in self.walk.interesting(&popped) {
            text.push_str(&format!(
                "# possible first '{}' commit: [{id}] {}\n",
                self.terms.bad,
                self.subject(&id)
            ));
        }
        self.walk.reset_walk();
        if self.append("BISECT_LOG", &text) {
            OK
        } else {
            error("could not open 'BISECT_LOG' for appending")
        }
    }

    fn bisect_checkout(&mut self, id: &ObjectId, no_checkout: bool) -> i32 {
        if self.set_pseudo_ref("BISECT_EXPECTED_REV", id).is_err() {
            die("could not write BISECT_EXPECTED_REV");
        }
        if no_checkout {
            if self.set_pseudo_ref("BISECT_HEAD", id).is_err() {
                die("could not write BISECT_HEAD");
            }
        } else if !self.checkout(&id.to_hex(), true, false) {
            return FAILED;
        }
        let line = format!("[{id}] {}\n", self.subject(id));
        self.say(&line);
        OK
    }

    fn join(ids: &[ObjectId]) -> String {
        ids.iter()
            .map(ObjectId::to_hex)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn check_good_are_ancestors(
        &mut self,
        bad: Option<ObjectId>,
        goods: &[ObjectId],
        skipped: &HashSet<ObjectId>,
        no_checkout: bool,
    ) -> i32 {
        let Some(bad) = bad else {
            return error(&format!("a '{}' revision is needed", self.terms.bad));
        };
        if self.path("BISECT_ANCESTORS_OK").is_file() || goods.is_empty() {
            return OK;
        }
        self.walk.configure(Pathspec::default(), false);
        let mut args = vec![(bad, true)];
        args.extend(goods.iter().map(|g| (*g, false)));
        let outside = match self.walk.limit(&args) {
            Ok(popped) => !popped.is_empty(),
            Err(e) => die(&format!("revision walk setup failed: {e}")),
        };
        self.walk.clear_marks();
        let bases = if outside {
            match merge_bases_many(&self.git.repo, &bad, goods) {
                Ok(b) => b,
                Err(_) => std::process::exit(128),
            }
        } else {
            Vec::new()
        };
        let mut res = OK;
        for mb in bases {
            if mb == bad {
                res = self.bad_merge_base(&bad, goods);
                break;
            } else if goods.contains(&mb) {
                continue;
            } else if skipped.contains(&mb) {
                eprintln!(
                    "warning: the merge base between {bad} and [{}] must be skipped.\nSo we cannot be sure the first '{}' commit is between {mb} and {bad}.\nWe continue anyway.",
                    Self::join(goods),
                    self.terms.bad
                );
            } else {
                self.say("Bisecting: a merge base must be tested\n");
                res = self.bisect_checkout(&mb, no_checkout);
                if res == OK {
                    res = MERGE_BASE_TESTED;
                }
                break;
            }
        }
        if res == OK && !self.write("BISECT_ANCESTORS_OK", "") {
            eprintln!("warning: could not create file 'BISECT_ANCESTORS_OK'");
        }
        res
    }

    fn bad_merge_base(&self, bad: &ObjectId, goods: &[ObjectId]) -> i32 {
        let (tb, tg) = (&self.terms.bad, &self.terms.good);
        let goods = Self::join(goods);
        if self.pseudo_ref("BISECT_EXPECTED_REV") == Some(*bad) {
            if tb == "bad" && tg == "good" {
                eprintln!(
                    "The merge base {bad} is bad.\nThis means the bug has been fixed between {bad} and [{goods}]."
                );
            } else if tb == "new" && tg == "old" {
                eprintln!(
                    "The merge base {bad} is new.\nThe property has changed between {bad} and [{goods}]."
                );
            } else {
                eprintln!(
                    "The merge base {bad} is '{tb}'.\nThis means the first '{tg}' commit is between {bad} and [{goods}]."
                );
            }
            return MERGE_BASE_CHECK;
        }
        eprintln!(
            "Some '{tg}' revs are not ancestors of the '{tb}' rev.\npyn bisect cannot work properly in this case.\nMaybe you mistook '{tg}' and '{tb}' revs?"
        );
        FAILED
    }

    fn pathspec(&self) -> Pathspec {
        let Some(text) = self.read("BISECT_NAMES") else {
            return Pathspec::default();
        };
        let mut paths = Vec::new();
        for line in text.split('\n') {
            let (words, ok) = sq_dequote(line.trim());
            if !ok {
                die(&format!(
                    "Badly quoted content in file 'BISECT_NAMES': {}",
                    line.trim()
                ));
            }
            paths.extend(words);
        }
        match self.git.pathspec(self.cwd, &paths, false) {
            Ok(spec) => spec,
            Err(e) => die(&e.to_string()),
        }
    }

    fn error_if_skipped(&mut self, tried: &[ObjectId], bad: Option<&ObjectId>) -> i32 {
        if tried.is_empty() {
            return OK;
        }
        let mut text = format!(
            "There are only 'skip'ped commits left to test.\nThe first '{}' commit could be any of:\n",
            self.terms.bad
        );
        for t in tried {
            text.push_str(&format!("{t}\n"));
        }
        if let Some(bad) = bad {
            text.push_str(&format!("{bad}\n"));
        }
        text.push_str("We cannot bisect more!\n");
        self.say(&text);
        ONLY_SKIPPED_LEFT
    }

    /// `managed_skipped`: the commit to test, away from the skipped ones, and the skipped ones passed over.
    fn pick_unskipped(
        list: &[ObjectId],
        skipped: &HashSet<ObjectId>,
        bad: &ObjectId,
    ) -> (Option<ObjectId>, Vec<ObjectId>) {
        if skipped.is_empty() {
            return (list.first().copied(), Vec::new());
        }
        let (mut tried, mut kept) = (Vec::new(), Vec::new());
        for id in list {
            if skipped.contains(id) {
                tried.push(*id);
            } else if tried.is_empty() {
                return (Some(*id), tried);
            } else {
                kept.push(*id);
            }
        }
        if kept.is_empty() {
            return (None, tried);
        }
        let at = skip_index(kept.len() as i32);
        let pick = match kept.get(at) {
            Some(c) if c != bad => *c,
            Some(_) if at > 0 => kept[at - 1],
            _ => kept[0],
        };
        (Some(pick), tried)
    }

    fn next_all(&mut self) -> i32 {
        let no_checkout = self.pseudo_ref("BISECT_HEAD").is_some();
        let (bad_term, good_term) = match self.read("BISECT_TERMS") {
            Some(text) => {
                let mut l = text.split('\n');
                (
                    l.next().unwrap_or_default().to_string(),
                    l.next().unwrap_or_default().to_string(),
                )
            }
            None => ("bad".to_string(), "good".to_string()),
        };
        self.terms = Terms {
            bad: bad_term,
            good: good_term,
        };
        let good_prefix = format!("{}-", self.terms.good);
        let (mut bad, mut goods, mut skipped) = (None, Vec::new(), Vec::new());
        for (name, id) in self.bisect_refs() {
            if name == self.terms.bad {
                bad = Some(id);
            } else if name.starts_with(&good_prefix) {
                goods.push(id);
            } else if name.starts_with("skip-") {
                skipped.push(id);
            }
        }
        let skipped: HashSet<ObjectId> = skipped.into_iter().collect();
        let first_parent = self.path("BISECT_FIRST_PARENT").exists();
        let res = self.check_good_are_ancestors(bad, &goods, &skipped, no_checkout);
        if res != OK {
            return res;
        }
        let bad = bad.expect("checked above");
        let spec = self.pathspec();
        self.walk.configure(spec, first_parent);
        let mut args = vec![(bad, false)];
        args.extend(goods.iter().map(|g| (*g, true)));
        let popped = match self.walk.limit(&args) {
            Ok(p) => p,
            Err(e) => die(&format!("revision walk setup failed: {e}")),
        };
        let found = self.walk.find_bisection(&popped, !skipped.is_empty());
        let (pick, tried) = Self::pick_unskipped(&found.list, &skipped, &bad);
        let Some(pick) = pick else {
            let res = self.error_if_skipped(&tried, None);
            if res < 0 {
                return res;
            }
            let text = format!(
                "{bad} was both '{}' and '{}'\n",
                self.terms.good, self.terms.bad
            );
            self.say(&text);
            return FAILED;
        };
        if found.all == 0 {
            eprintln!("No testable commit found.\nMaybe you started with bad path arguments?");
            return NO_TESTABLE_COMMIT;
        }
        if pick == bad {
            let res = self.error_if_skipped(&tried, Some(&bad));
            if res != OK {
                return res;
            }
            let text = format!("{bad} is the first '{}' commit\n", self.terms.bad);
            self.say(&text);
            self.show_commit(&bad);
            return FIRST_BAD_FOUND;
        }
        let nr = found.all - found.reaches - 1;
        let steps = estimate_steps(found.all);
        let steps_msg = format!(
            "(roughly {steps} step{})",
            if steps == 1 { "" } else { "s" }
        );
        let text = format!(
            "Bisecting: {nr} revision{} left to test after this {steps_msg}\n",
            if nr == 1 { "" } else { "s" }
        );
        self.say(&text);
        self.walk.clear_marks();
        self.bisect_checkout(&pick, no_checkout)
    }

    /// What `git show --stat --summary --no-abbrev-commit --diff-merges=first-parent` prints for `id`.
    fn show_commit(&mut self, id: &ObjectId) {
        let git = self.git;
        let rendered = (|| -> Result<String> {
            let c = git.repo.commit(id)?;
            let Rendering { pretty, style } = FormatArgs::default().rendering()?;
            let names = Decorations::new();
            let ctx = Context {
                style,
                now: chrono::Utc::now().timestamp(),
                decorations: &names,
                commit_color: None,
            };
            let mut out = render(&pretty, &ctx, id, &c);
            out.push_str(pretty.terminator());
            let old_tree = match c.parents.first() {
                Some(p) => Some(git.repo.commit(p)?.tree),
                None => None,
            };
            let (mut old, mut new) = (Tree::new(), Tree::new());
            let changes =
                git.repo
                    .diff_trees(old_tree.as_ref(), Some(&c.tree), &Pathspec::default())?;
            add_changes(git, &mut old, &mut new, &changes)?;
            let graph = git
                .config()
                .get("diff.statgraphwidth")
                .and_then(|w| w.parse::<usize>().ok());
            let stat = diff::merge_stat(git, &old, &new, (diffstat::terminal_width(), graph))?;
            if !stat.is_empty() {
                out.push('\n');
                out.push_str(&stat);
            }
            Ok(out)
        })();
        match rendered {
            Ok(text) => self.say(&text),
            Err(e) => die(&format!("unable to start 'show' for object '{id}': {e}")),
        }
    }

    fn start(&mut self, argv: &[String]) -> i32 {
        let has_double_dash = argv.iter().any(|a| a == "--");
        let (mut no_checkout, mut first_parent, mut must_write_terms) =
            (self.git.is_bare(), false, false);
        let mut revs: Vec<ObjectId> = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            let arg = argv[i].as_str();
            if arg == "--" {
                break;
            } else if arg == "--no-checkout" {
                no_checkout = true;
            } else if arg == "--first-parent" {
                first_parent = true;
            } else if matches!(
                arg,
                "--term-good" | "--term-old" | "--term-bad" | "--term-new"
            ) {
                i += 1;
                let Some(value) = argv.get(i) else {
                    return error("'' is not a valid term");
                };
                must_write_terms = true;
                if matches!(arg, "--term-good" | "--term-old") {
                    self.terms.good = value.clone();
                } else {
                    self.terms.bad = value.clone();
                }
            } else if let Some((name, value)) = arg.split_once('=')
                && matches!(
                    name,
                    "--term-good" | "--term-old" | "--term-bad" | "--term-new"
                )
            {
                must_write_terms = true;
                if matches!(name, "--term-good" | "--term-old") {
                    self.terms.good = value.to_string();
                } else {
                    self.terms.bad = value.to_string();
                }
            } else if arg.starts_with("--") {
                return error(&format!("unrecognized option: '{arg}'"));
            } else if let Some(id) = self.commit_of(arg) {
                revs.push(id);
            } else if has_double_dash {
                die(&format!("'{arg}' does not appear to be a valid revision"));
            } else {
                break;
            }
            i += 1;
        }
        let pathspec_pos = i;
        if !revs.is_empty() {
            must_write_terms = true;
        }
        let states: Vec<String> = (0..revs.len())
            .map(|n| {
                if n == 0 {
                    self.terms.bad.clone()
                } else {
                    self.terms.good.clone()
                }
            })
            .collect();
        let head = match self.git.repo.head() {
            Ok(h) => h,
            Err(_) => return error("bad HEAD - I need a HEAD"),
        };
        let start_head = if self.not_empty("BISECT_START") {
            let text = self.read("BISECT_START").unwrap_or_default();
            let start = text.trim().to_string();
            if !no_checkout && !self.checkout(&start, false, false) {
                return error(&format!(
                    "checking out '{start}' failed. Try 'pyn bisect start <valid-branch>'."
                ));
            }
            start
        } else {
            match head {
                Head::Detached(id) => id.to_hex(),
                Head::Branch { name, id: Some(_) } => name,
                Head::Branch { .. } => return error("bad HEAD - strange symbolic ref"),
            }
        };
        if self.clean_state() != OK {
            return FAILED;
        }
        self.write("BISECT_START", &format!("{start_head}\n"));
        if first_parent {
            self.write("BISECT_FIRST_PARENT", "\n");
        }
        if no_checkout {
            let Some(id) = self.commit_of(&start_head) else {
                return error(&format!("invalid ref: '{start_head}'"));
            };
            if self.set_pseudo_ref("BISECT_HEAD", &id).is_err() {
                return FAILED;
            }
        }
        let names = if pathspec_pos + 1 < argv.len() {
            sq_quote_argv(&argv[pathspec_pos..])
        } else {
            String::new()
        };
        self.write("BISECT_NAMES", &format!("{names}\n"));
        for (state, rev) in states.iter().zip(&revs) {
            if self.write_state(state, &rev.to_hex(), true) != OK {
                return FAILED;
            }
        }
        if must_write_terms {
            let (bad, good) = (self.terms.bad.clone(), self.terms.good.clone());
            if self.write_terms(&bad, &good) != OK {
                return FAILED;
            }
        }
        let logged = format!("git bisect start{}\n", sq_quote_argv(argv));
        if !self.append("BISECT_LOG", &logged) {
            return FAILED;
        }
        let res = self.auto_next();
        if !matches!(res, OK | FIRST_BAD_FOUND | MERGE_BASE_TESTED) {
            self.clean_state();
        }
        res
    }

    fn state(&mut self, argv: &[String]) -> i32 {
        if self.autostart() != OK {
            return FAILED;
        }
        let state = argv[0].clone();
        let words = &argv[1..];
        if self.check_and_set_terms(&state) != OK
            || !(state == self.terms.good || state == self.terms.bad || state == "skip")
        {
            return FAILED;
        }
        if words.len() > 1 && state == self.terms.bad {
            return error(&format!(
                "'pyn bisect {}' can take only one argument.",
                self.terms.bad
            ));
        }
        let mut revs: Vec<ObjectId> = Vec::new();
        if words.is_empty() {
            let id = self
                .pseudo_ref("BISECT_HEAD")
                .or_else(|| self.any_object("HEAD"));
            match id {
                Some(id) => revs.push(id),
                None => return error("Bad rev input: HEAD"),
            }
        }
        for w in words {
            let Some(id) = self.any_object(w) else {
                error(&format!("Bad rev input: {w}"));
                return FAILED;
            };
            match super::plumbing::peel(self.git, &id, Some(pyn_object::Kind::Commit)) {
                Ok(Some(c)) => revs.push(c),
                _ => die(&format!("Bad rev input (not a commit): {w}")),
            }
        }
        let mut expected = self.pseudo_ref("BISECT_EXPECTED_REV");
        for id in &revs {
            if self.write_state(&state, &id.to_hex(), false) != OK {
                return FAILED;
            }
            if expected.is_some_and(|e| e != *id) {
                self.unlink("BISECT_ANCESTORS_OK");
                self.unlink("BISECT_EXPECTED_REV");
                expected = None;
            }
        }
        self.auto_next()
    }

    fn skip(&mut self, args: &[String]) -> i32 {
        let mut argv = vec!["skip".to_string()];
        for a in args {
            let Some((from, to)) = a.split_once("..") else {
                argv.push(a.clone());
                continue;
            };
            if to.starts_with('.') {
                eprintln!("error: `pyn bisect skip` takes a range of the form <a>..<b>");
                std::process::exit(129);
            }
            let (Some(start), Some(end)) = (self.range_end(from), self.range_end(to)) else {
                die(&format!("bad revision '{a}'"))
            };
            self.walk.configure(Pathspec::default(), false);
            let Ok(popped) = self.walk.limit(&[(start, true), (end, false)]) else {
                die("revision walk setup failed");
            };
            argv.extend(self.walk.interesting(&popped).iter().map(ObjectId::to_hex));
            self.walk.reset_walk();
        }
        self.state(&argv)
    }

    fn range_end(&self, spec: &str) -> Option<ObjectId> {
        self.commit_of(if spec.is_empty() { "HEAD" } else { spec })
    }

    fn log(&mut self) -> i32 {
        if !self.not_empty("BISECT_LOG") {
            return error("We are not bisecting.");
        }
        let text = self.read("BISECT_LOG").unwrap_or_default();
        self.say(&text);
        OK
    }

    fn replay(&mut self, file: &str) -> i32 {
        let path = self.cwd.join(file);
        let Some(text) = std::fs::read(&path)
            .ok()
            .filter(|b| !b.is_empty())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
        else {
            return error(&format!("cannot read file '{file}' for replaying"));
        };
        if self.reset(None) != OK {
            return FAILED;
        }
        let mut res = OK;
        let mut lines: Vec<&str> = text.split('\n').collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        for line in lines {
            if res != OK {
                break;
            }
            res = self.replay_line(line.strip_suffix('\r').unwrap_or(line));
        }
        if res != OK {
            return FAILED;
        }
        self.auto_next()
    }

    fn replay_line(&mut self, line: &str) -> i32 {
        let p = line.trim_start_matches([' ', '\t']);
        let Some(p) = p
            .strip_prefix("git bisect")
            .or_else(|| p.strip_prefix("git-bisect"))
            .filter(|r| r.starts_with(|c: char| c.is_ascii_whitespace()))
        else {
            return OK;
        };
        let p = p.trim_start_matches([' ', '\t']);
        let word_end = p.find([' ', '\t']).unwrap_or(p.len());
        let (word, rest) = p.split_at(word_end);
        let rev = rest.trim_start_matches([' ', '\t']);
        self.get_terms();
        if self.check_and_set_terms(word) != OK {
            return FAILED;
        }
        if word == "start" {
            let (args, _) = sq_dequote(rev);
            return self.start(&args);
        }
        if word == self.terms.good || word == self.terms.bad || word == "skip" {
            return self.write_state(word, rev, false);
        }
        if word == "terms" {
            let (args, _) = sq_dequote(rev);
            return self.terms_command(if args.len() == 1 {
                Some(args[0].as_str())
            } else {
                None
            });
        }
        error(&format!("'{word}'?? what are you talking about?"))
    }

    fn do_run(&self, command: &str) -> i32 {
        println!("running {command}");
        let _ = std::io::stdout().flush();
        let root = self.git.repo.root().map(Path::to_path_buf).ok();
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        if let Some(root) = root {
            cmd.current_dir(root);
        }
        match cmd.status() {
            Ok(status) => {
                use std::os::unix::process::ExitStatusExt;
                status
                    .code()
                    .or_else(|| status.signal().map(|s| s + 128))
                    .unwrap_or(FAILED)
            }
            Err(_) => 127,
        }
    }

    fn verify_good(&mut self, command: &str) -> i32 {
        let no_checkout = self.pseudo_ref("BISECT_HEAD").is_some();
        let good = format!("{}-", self.terms.good);
        let Some((_, good_rev)) = self
            .bisect_refs()
            .into_iter()
            .find(|(n, _)| n.starts_with(&good))
        else {
            return -1;
        };
        let current = if no_checkout {
            self.pseudo_ref("BISECT_HEAD")
        } else {
            self.git.repo.head_commit().ok().flatten()
        };
        let Some(current) = current else {
            return -1;
        };
        if self.bisect_checkout(&good_rev, no_checkout) != OK {
            return -1;
        }
        let rc = self.do_run(command);
        if self.bisect_checkout(&current, no_checkout) != OK {
            return -1;
        }
        rc
    }

    fn run(&mut self, argv: &[String]) -> i32 {
        if self.next_check(None) != OK {
            return FAILED;
        }
        let command = sq_quote_argv(argv).trim_start().to_string();
        let mut is_first_run = true;
        loop {
            let mut res = self.do_run(&command);
            if is_first_run && (res == 126 || res == 127) {
                let rc = self.verify_good(&command);
                is_first_run = false;
                if !(0..128).contains(&rc) {
                    error(&format!(
                        "unable to verify {command} on '{}' revision",
                        self.terms.good
                    ));
                    return FAILED;
                }
                if rc == res {
                    error(&format!(
                        "bogus exit code {rc} for '{}' revision",
                        self.terms.good
                    ));
                    return FAILED;
                }
            }
            if !(0..128).contains(&res) {
                error(&format!(
                    "bisect run failed: exit code {res} from {command} is < 0 or >= 128"
                ));
                return res;
            }
            let new_state = if res == 125 {
                "skip".to_string()
            } else if res == 0 {
                self.terms.good.clone()
            } else {
                self.terms.bad.clone()
            };
            self.capture = Some(String::new());
            res = self.state(std::slice::from_ref(&new_state));
            let captured = self.capture.take().unwrap_or_default();
            self.write("BISECT_RUN", &captured);
            print!("{captured}");
            let _ = std::io::stdout().flush();
            match res {
                ONLY_SKIPPED_LEFT => {
                    error("bisect run cannot continue any more");
                    return res;
                }
                MERGE_BASE_TESTED => {
                    println!("bisect run success");
                    return OK;
                }
                FIRST_BAD_FOUND => {
                    println!("bisect found first '{}' commit", self.terms.bad);
                    return OK;
                }
                OK => {}
                other => {
                    error(&format!(
                        "bisect run failed: 'pyn bisect {new_state}' exited with error code {other}"
                    ));
                    return other;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_round_trips() {
        let args = vec!["a b".to_string(), "it's".to_string(), "x!y".to_string()];
        let quoted = sq_quote_argv(&args);
        assert_eq!(quoted, " 'a b' 'it'\\''s' 'x'\\!'y'");
        assert_eq!(sq_dequote(quoted.trim()), (args, true));
        assert!(!sq_dequote("abc").1);
    }

    #[test]
    fn steps_follow_gits_estimate() {
        let steps: Vec<i32> = (0..=12).map(estimate_steps).collect();
        assert_eq!(steps, [0, 0, 0, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3]);
    }
}

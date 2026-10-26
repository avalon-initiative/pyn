//! `pyn grep` on a git repository: `git grep`'s matching and output over the work tree, the index and revisions.

use std::ffi::{OsStr, OsString};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use clap::Args;
use pyn_gitwork::fs_path;
use pyn_object::{FileMode, ObjectId};
use regex::bytes::Regex;

use super::GitHome;
use super::clean::relative;
use super::grep_regex::{Syntax, compile};

const USAGE: &str = "usage: pyn grep [<options>] [-e] <pattern> [<rev>...] [[--] <path>...]";
const BINARY_SCAN: usize = 8000;

const HELP: &str = "\
Options (a git repository only; output, order and exit status are git grep's):
  -e <pattern>          match <pattern>; repeat it, or join patterns with --and, --or, --not and ( )
  -G, -E, -F            basic (default), extended or fixed-string patterns
  -i  -w  -v            ignore case, match whole words, show non-matching lines
  -n  -h  -H            show line numbers, hide or show file names
  -l  -c  -q            only file names, only counts, nothing but the exit status
  -a  -I                treat binary files as text, skip binary files
  --cached              search the index instead of the work tree
  --full-name           show paths from the top of the repository
Paths after the revisions (or after --) limit the search; with revisions the files of each commit are searched.
Exit status: 0 when something matched, 1 when nothing did.";

#[derive(Args, Debug)]
#[command(disable_help_flag = true, after_help = HELP)]
pub struct GrepArgs {
    #[arg(long, action = clap::ArgAction::Help, hide = true)]
    help: Option<bool>,
    /// Options, the pattern, revisions and paths, as for `git grep`.
    #[arg(
        allow_hyphen_values = true,
        trailing_var_arg = true,
        value_name = "ARGS"
    )]
    args: Vec<OsString>,
}

#[derive(Default)]
struct Options {
    cached: bool,
    invert: bool,
    icase: bool,
    word: bool,
    text: bool,
    skip_binary: bool,
    syntax: Option<Syntax>,
    line_numbers: Option<bool>,
    no_filename: bool,
    full_name: Option<bool>,
    list: bool,
    count: bool,
    quiet: bool,
}

enum Token {
    Pattern(Vec<u8>, &'static str),
    And,
    Not,
    Open,
    Close,
}

fn usage(message: &str) -> ! {
    eprintln!("error: {message}\n{USAGE}");
    std::process::exit(129);
}

fn os_bytes(s: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        s.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        s.to_string_lossy().into_owned().into_bytes()
    }
}

/// The arguments after the `grep` word as typed, since the argument parser drops a `--`.
fn raw_args() -> Vec<OsString> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--server" | "--token" | "--user" | "--repo") => {
                args.next();
            }
            Some(a) if a.starts_with('-') => {}
            _ => break,
        }
    }
    args.collect()
}

const UNSUPPORTED_LONG: [&str; 24] = [
    "untracked",
    "no-index",
    "index",
    "exclude-standard",
    "recurse-submodules",
    "textconv",
    "max-depth",
    "column",
    "null",
    "only-matching",
    "color",
    "break",
    "heading",
    "context",
    "before-context",
    "after-context",
    "show-function",
    "function-context",
    "perl-regexp",
    "all-match",
    "max-count",
    "threads",
    "open-files-in-pager",
    "files-without-match",
];

fn long_option(name: &str, opts: &mut Options, tokens: &mut Vec<Token>) {
    let flag = |on: &mut bool| *on = true;
    match name {
        "cached" => flag(&mut opts.cached),
        "line-number" => opts.line_numbers = Some(true),
        "no-line-number" => opts.line_numbers = Some(false),
        "ignore-case" => flag(&mut opts.icase),
        "word-regexp" => flag(&mut opts.word),
        "files-with-matches" | "name-only" => flag(&mut opts.list),
        "count" => flag(&mut opts.count),
        "invert-match" => flag(&mut opts.invert),
        "extended-regexp" => opts.syntax = Some(Syntax::Extended),
        "basic-regexp" => opts.syntax = Some(Syntax::Basic),
        "fixed-strings" => opts.syntax = Some(Syntax::Fixed),
        "quiet" => flag(&mut opts.quiet),
        "text" => flag(&mut opts.text),
        "recursive" => {}
        "full-name" => opts.full_name = Some(true),
        "no-full-name" => opts.full_name = Some(false),
        "and" => tokens.push(Token::And),
        "or" => {}
        "not" => tokens.push(Token::Not),
        _ if UNSUPPORTED_LONG.contains(&name) => {
            usage(&format!("`pyn grep --{name}` is not supported"))
        }
        _ => usage(&format!("unknown option `{name}'")),
    }
}

fn short_option(c: char, opts: &mut Options) {
    match c {
        'n' => opts.line_numbers = Some(true),
        'i' => opts.icase = true,
        'w' => opts.word = true,
        'l' => opts.list = true,
        'c' => opts.count = true,
        'v' => opts.invert = true,
        'E' => opts.syntax = Some(Syntax::Extended),
        'G' => opts.syntax = Some(Syntax::Basic),
        'F' => opts.syntax = Some(Syntax::Fixed),
        'h' => opts.no_filename = true,
        'H' => opts.no_filename = false,
        'q' => opts.quiet = true,
        'a' => opts.text = true,
        'I' => opts.skip_binary = true,
        'r' => {}
        'A' | 'B' | 'C' | 'p' | 'W' | 'o' | 'z' | 'f' | 'O' | 'm' | 'P' | 'L' | '0'..='9' => {
            usage(&format!("`pyn grep -{c}` is not supported"))
        }
        _ => usage(&format!("unknown switch `{c}'")),
    }
}

/// Reads the options, which end at the first argument that is not one; returns how many arguments they used.
fn parse_options(args: &[OsString], opts: &mut Options, tokens: &mut Vec<Token>) -> usize {
    let mut i = 0;
    while i < args.len() {
        let Some(arg) = args[i].to_str() else { break };
        match arg {
            "--" => break,
            "(" => tokens.push(Token::Open),
            ")" => tokens.push(Token::Close),
            _ if arg.starts_with("--") => {
                let name = arg[2..].split_once('=').map_or(&arg[2..], |(n, _)| n);
                long_option(name, opts, tokens);
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                let mut cluster = arg[1..].chars();
                while let Some(c) = cluster.next() {
                    if c != 'e' {
                        short_option(c, opts);
                        continue;
                    }
                    let attached = cluster.as_str();
                    let value = if attached.is_empty() {
                        i += 1;
                        match args.get(i) {
                            Some(v) => os_bytes(v),
                            None => usage("switch `e' requires a value"),
                        }
                    } else {
                        attached.as_bytes().to_vec()
                    };
                    tokens.push(Token::Pattern(value, "-e option"));
                    break;
                }
            }
            _ => break,
        }
        i += 1;
    }
    i
}

enum Expr {
    Atom(usize),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy)]
enum Tok {
    Atom(usize),
    And,
    Not,
    Open,
    Close,
}

/// Joins the tokens as git does: `--or` and juxtaposition are the loosest, then `--and`, then `--not`.
struct ExprParser {
    toks: Vec<Tok>,
    at: usize,
}

impl ExprParser {
    fn peek(&self) -> Option<Tok> {
        self.toks.get(self.at).copied()
    }

    fn or(&mut self) -> Result<Option<Expr>> {
        let Some(mut left) = self.and()? else {
            return Ok(None);
        };
        while !matches!(self.peek(), None | Some(Tok::Close)) {
            let right = self
                .and()?
                .ok_or_else(|| anyhow!("not a pattern expression"))?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(Some(left))
    }

    fn and(&mut self) -> Result<Option<Expr>> {
        let Some(mut left) = self.not()? else {
            return Ok(None);
        };
        while matches!(self.peek(), Some(Tok::And)) {
            self.at += 1;
            let right = self
                .not()?
                .ok_or_else(|| anyhow!("--and not followed by pattern expression"))?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(Some(left))
    }

    fn not(&mut self) -> Result<Option<Expr>> {
        let Some(tok) = self.peek() else {
            return Ok(None);
        };
        match tok {
            Tok::Not => {
                self.at += 1;
                let inner = self
                    .not()?
                    .ok_or_else(|| anyhow!("--not not followed by pattern expression"))?;
                Ok(Some(Expr::Not(Box::new(inner))))
            }
            Tok::Open => {
                self.at += 1;
                let inner = self.or()?;
                if !matches!(self.peek(), Some(Tok::Close)) {
                    bail!("unmatched parenthesis");
                }
                self.at += 1;
                match inner {
                    Some(e) => Ok(Some(e)),
                    None => bail!("not a pattern expression ("),
                }
            }
            Tok::And => bail!("--and not preceded by pattern expression"),
            Tok::Atom(i) => {
                self.at += 1;
                Ok(Some(Expr::Atom(i)))
            }
            Tok::Close => Ok(None),
        }
    }
}

struct Matcher {
    re: Regex,
    word: bool,
}

fn word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl Matcher {
    fn matches(&self, line: &[u8]) -> bool {
        if !self.word {
            return self.re.is_match(line);
        }
        let mut from = 0;
        while let Some(m) = self.re.find_at(line, from) {
            let (start, end) = (m.start(), m.end());
            let bounded = (start == 0 || !word_byte(line[start - 1]))
                && (end == line.len() || !word_byte(line[end]));
            if bounded && start != end {
                return true;
            }
            if start + 1 >= line.len() {
                return false;
            }
            from = start + 1;
            while from < line.len() && word_byte(line[from - 1]) {
                from += 1;
            }
            if from >= line.len() {
                return false;
            }
        }
        false
    }
}

struct Query {
    matchers: Vec<Matcher>,
    expr: Expr,
    invert: bool,
    /// git scans a plain pattern list a buffer at a time, so an empty-line match also hits the nothing after a
    /// final newline when the last line did not match.
    trailing_empty: bool,
}

impl Query {
    fn eval(&self, e: &Expr, line: &[u8]) -> bool {
        match e {
            Expr::Atom(i) => self.matchers[*i].matches(line),
            Expr::Not(x) => !self.eval(x, line),
            Expr::And(a, b) => self.eval(a, line) && self.eval(b, line),
            Expr::Or(a, b) => self.eval(a, line) || self.eval(b, line),
        }
    }

    fn selected(&self, line: &[u8]) -> bool {
        self.eval(&self.expr, line) != self.invert
    }
}

fn syntax_of(git: &GitHome, opts: &Options) -> Result<Syntax> {
    if let Some(s) = opts.syntax {
        return Ok(s);
    }
    let config = git.repo.config();
    match config.get("grep.patterntype") {
        Some("basic") => return Ok(Syntax::Basic),
        Some("extended") => return Ok(Syntax::Extended),
        Some("fixed") => return Ok(Syntax::Fixed),
        Some("perl") => bail!("grep.patternType=perl is not supported"),
        Some("default" | "") | None => {}
        Some(other) => bail!("bad config variable 'grep.patterntype': {other}"),
    }
    Ok(if git.repo.bool_setting("grep.extendedregexp", false) {
        Syntax::Extended
    } else {
        Syntax::Basic
    })
}

/// Splits patterns at newlines (each line is a pattern of its own) and compiles them.
fn build_query(git: &GitHome, opts: &Options, tokens: &[Token]) -> Result<Query> {
    let syntax = syntax_of(git, opts)?;
    let mut matchers = Vec::new();
    let mut toks = Vec::new();
    for token in tokens {
        match token {
            Token::Pattern(bytes, origin) => {
                for piece in bytes.split(|b| *b == b'\n') {
                    let text = std::str::from_utf8(piece)
                        .map_err(|_| anyhow!("{origin}: patterns must be valid UTF-8"))?;
                    let re = compile(text, syntax, opts.icase)
                        .map_err(|e| anyhow!("{origin}, '{text}': {e}"))?;
                    toks.push(Tok::Atom(matchers.len()));
                    matchers.push(Matcher {
                        re,
                        word: opts.word,
                    });
                }
            }
            Token::And => toks.push(Tok::And),
            Token::Not => toks.push(Tok::Not),
            Token::Open => toks.push(Tok::Open),
            Token::Close => toks.push(Tok::Close),
        }
    }
    let mut parser = ExprParser { toks, at: 0 };
    let expr = parser.or()?;
    if matches!(parser.peek(), Some(Tok::Close)) {
        bail!("incomplete pattern expression: )");
    }
    let expr = expr.ok_or_else(|| anyhow!("no pattern given"))?;
    let plain = tokens.iter().all(|t| matches!(t, Token::Pattern(..)));
    let trailing_empty =
        plain && !opts.invert && !opts.word && matchers.iter().any(|m| m.re.is_match(b""));
    Ok(Query {
        matchers,
        expr,
        invert: opts.invert,
        trailing_empty,
    })
}

struct Targets {
    revs: Vec<(String, ObjectId)>,
    paths: Vec<String>,
}

fn utf8(arg: &OsStr) -> Result<&str> {
    arg.to_str()
        .ok_or_else(|| anyhow!("{}: not valid UTF-8", arg.to_string_lossy()))
}

/// What follows the pattern: revisions, then paths, as git tells them apart.
fn targets(git: &GitHome, cwd: &Path, rest: &[OsString]) -> Result<Targets> {
    let mut found = Targets {
        revs: Vec::new(),
        paths: Vec::new(),
    };
    let exists = |name: &str| git.cwd_has(cwd, name);
    if let Some(dashes) = rest.iter().position(|a| a == "--") {
        for arg in &rest[..dashes] {
            let text = utf8(arg)?;
            let id = git
                .repo
                .resolve(text)
                .map_err(|_| anyhow!("unable to resolve revision: {text}"))?;
            found.revs.push((text.to_string(), id));
        }
        for arg in &rest[dashes + 1..] {
            found.paths.push(utf8(arg)?.to_string());
        }
        return Ok(found);
    }
    let mut i = 0;
    while i < rest.len() {
        let text = utf8(&rest[i])?;
        let Ok(id) = git.repo.resolve(text) else {
            break;
        };
        if exists(text) {
            bail!(
                "ambiguous argument '{text}': both revision and filename\n\
                 Use '--' to separate paths from revisions, like this:\n\
                 'git <command> [<revision>...] -- [<file>...]'"
            );
        }
        found.revs.push((text.to_string(), id));
        i += 1;
    }
    for (n, arg) in rest[i..].iter().enumerate() {
        let text = utf8(arg)?;
        if text.starts_with('-') {
            bail!("option '{text}' must come before non-option arguments");
        }
        if !exists(text) && !super::stage::looks_like_pathspec(text) {
            if n == 0 {
                bail!(
                    "ambiguous argument '{text}': unknown revision or path not in the working tree.\n\
                     Use '--' to separate paths from revisions, like this:\n\
                     'git <command> [<revision>...] -- [<file>...]'"
                );
            }
            bail!(
                "{text}: no such path in the working tree.\n\
                 Use 'git <command> -- <path>...' to specify paths that do not exist locally."
            );
        }
        found.paths.push(text.to_string());
    }
    Ok(found)
}

struct Searcher<'a> {
    query: &'a Query,
    opts: &'a Options,
    line_numbers: bool,
    out: BufWriter<std::io::StdoutLock<'static>>,
    matched: bool,
}

impl Searcher<'_> {
    fn put(&mut self, bytes: &[u8]) -> Result<()> {
        if let Err(e) = self.out.write_all(bytes) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                std::process::exit(0);
            }
            return Err(e.into());
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.opts.quiet && self.matched
    }

    fn file(&mut self, name: &[u8], data: &[u8]) -> Result<()> {
        let binary = !self.opts.text && data[..data.len().min(BINARY_SCAN)].contains(&0);
        if binary && self.opts.skip_binary {
            return Ok(());
        }
        let (mut count, mut number) = (0usize, 0usize);
        let mut rest = data;
        let mut trailing = false;
        loop {
            let line = if !rest.is_empty() {
                let (line, tail) = match rest.iter().position(|b| *b == b'\n') {
                    Some(i) => (&rest[..i], &rest[i + 1..]),
                    None => (rest, &rest[rest.len()..]),
                };
                rest = tail;
                line
            } else if trailing {
                trailing = false;
                &rest[..0]
            } else {
                break;
            };
            number += 1;
            let selected = self.query.selected(line);
            if rest.is_empty() && !selected && data.ends_with(b"\n") && self.query.trailing_empty {
                trailing = true;
            }
            if !selected {
                continue;
            }
            self.matched = true;
            count += 1;
            if self.opts.quiet {
                return Ok(());
            }
            if self.opts.list {
                self.put(name)?;
                return self.put(b"\n");
            }
            if self.opts.count {
                continue;
            }
            if binary {
                self.put(b"Binary file ")?;
                self.put(name)?;
                return self.put(b" matches\n");
            }
            let mut text = Vec::new();
            if !self.opts.no_filename {
                text.extend_from_slice(name);
                text.push(b':');
            }
            if self.line_numbers {
                text.extend_from_slice(format!("{number}:").as_bytes());
            }
            text.extend_from_slice(line);
            text.push(b'\n');
            self.put(&text)?;
        }
        if self.opts.count && !self.opts.list && count > 0 {
            let mut text = Vec::new();
            if !self.opts.no_filename {
                text.extend_from_slice(name);
                text.push(b':');
            }
            text.extend_from_slice(format!("{count}\n").as_bytes());
            self.put(&text)?;
        }
        Ok(())
    }
}

fn regular(mode: u32) -> bool {
    mode & 0o170000 == 0o100000
}

/// Searches the work tree, the index or revisions; true when anything matched.
pub fn grep(git: &GitHome, cwd: &Path) -> Result<bool> {
    let args = raw_args();
    let mut opts = Options::default();
    let mut tokens = Vec::new();
    let used = parse_options(&args, &mut opts, &mut tokens);
    let mut rest = &args[used..];
    if tokens.is_empty() {
        if rest.first().is_some_and(|a| a == "--") {
            rest = &rest[1..];
        }
        let Some((pattern, tail)) = rest.split_first() else {
            bail!("no pattern given");
        };
        tokens.push(Token::Pattern(os_bytes(pattern), "command line"));
        rest = tail;
    }
    let found = targets(git, cwd, rest)?;
    let query = build_query(git, &opts, &tokens)?;
    if opts.cached && !found.revs.is_empty() {
        bail!("both --cached and trees are given");
    }

    let prefix = git.ws.repo_prefix(cwd, ".")?;
    let spec = git.pathspec(cwd, &found.paths, true)?;

    let line_numbers = opts
        .line_numbers
        .unwrap_or_else(|| git.repo.bool_setting("grep.linenumber", false));
    let full_name = opts
        .full_name
        .unwrap_or_else(|| git.repo.bool_setting("grep.fullname", false));
    let mut searcher = Searcher {
        query: &query,
        opts: &opts,
        line_numbers,
        out: BufWriter::new(std::io::stdout().lock()),
        matched: false,
    };
    let quote_high = git.repo.bool_setting("core.quotepath", true);
    let shown = |path: &[u8]| {
        let name = if full_name {
            path.to_vec()
        } else {
            relative(path, prefix.as_bytes())
        };
        crate::short_status::quote_diff(&name, quote_high).into_bytes()
    };

    if found.revs.is_empty() {
        let index = git.repo.read_index()?;
        let root = if opts.cached {
            PathBuf::new()
        } else {
            git.repo.root()?.to_path_buf()
        };
        let mut last: Option<&[u8]> = None;
        for e in &index.entries {
            if searcher.done() {
                break;
            }
            if !regular(e.mode) || !spec.matches(&e.path) {
                continue;
            }
            if opts.cached {
                if e.stage != 0 || e.intent_to_add {
                    continue;
                }
                searcher.file(&shown(&e.path), &git.repo.blob(&e.id)?)?;
                continue;
            }
            if last == Some(e.path.as_slice()) {
                continue;
            }
            last = Some(&e.path);
            if e.assume_valid || e.skip_worktree {
                if e.stage == 0 {
                    searcher.file(&shown(&e.path), &git.repo.blob(&e.id)?)?;
                }
                continue;
            }
            let on_disk = fs_path(&root, &e.path);
            if !std::fs::symlink_metadata(&on_disk).is_ok_and(|m| m.is_file()) {
                continue;
            }
            match std::fs::read(&on_disk) {
                Ok(data) => searcher.file(&shown(&e.path), &data)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    for (name, id) in &found.revs {
        let tree = git.repo.commit(id)?.tree;
        for (path, entry) in git.repo.flatten(Some(&tree), &spec)? {
            if searcher.done() {
                break;
            }
            if !matches!(entry.mode, FileMode::Regular | FileMode::Executable) {
                continue;
            }
            let mut label = format!("{name}:").into_bytes();
            label.extend_from_slice(&shown(&path));
            searcher.file(&label, &git.repo.blob(&entry.id)?)?;
        }
    }
    searcher.out.flush()?;
    Ok(searcher.matched)
}

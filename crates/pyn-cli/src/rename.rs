//! Rename and copy detection as git's diffcore does it: exact matches, same-name guesses, then a similarity matrix.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;

use crate::backend::Backend;
use crate::diff::{Blob, Tree, blob_bytes};

pub const MAX_SCORE: u64 = 60_000;
pub const DEFAULT_SCORE: u64 = 30_000;
pub const DEFAULT_LIMIT: usize = 1000;
const HASHBASE: u32 = 107_927;
const CANDIDATES: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Renames,
    Copies,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub mode: Mode,
    /// Minimum similarity out of `MAX_SCORE`.
    pub score: u64,
    /// `diff.renameLimit`; 0 or less means unlimited.
    pub limit: i64,
    /// `-C -C`: unmodified files are copy sources too, so callers pass whole trees.
    pub harder: bool,
}

impl Options {
    pub fn renames() -> Self {
        Self {
            mode: Mode::Renames,
            score: DEFAULT_SCORE,
            limit: DEFAULT_LIMIT as i64,
            harder: false,
        }
    }
}

/// What `diff.renames` and `diff.renameLimit` say, which the flags override.
#[derive(Clone, Copy, Debug)]
pub struct Defaults {
    pub mode: Option<Mode>,
    pub limit: i64,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            mode: None,
            limit: DEFAULT_LIMIT as i64,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Choice {
    #[default]
    Config,
    Off,
    On(Mode),
}

/// The detection the command line asks for; flags given later win, as in git.
#[derive(Clone, Copy, Debug, Default)]
pub struct Request {
    pub choice: Choice,
    pub score: Option<u64>,
    pub limit: Option<i64>,
    /// `--find-copies-harder`, or a second `-C`.
    pub harder: bool,
    /// A `-C` given while `diff.renames` alone decided the mode: `-C -C` if that was `copies`.
    over_config: bool,
}

impl Request {
    /// Whether unmodified files are copy sources too.
    pub fn is_harder(&self, defaults: Defaults) -> bool {
        self.harder || (self.over_config && defaults.mode == Some(Mode::Copies))
    }

    /// The options in force; git reads `--find-copies-harder` as copy detection whatever else is given.
    pub fn resolve(&self, defaults: Defaults) -> Result<Option<Options>, String> {
        let harder = self.is_harder(defaults);
        let mode = match self.choice {
            _ if harder => Mode::Copies,
            Choice::Off => return Ok(None),
            Choice::On(m) => m,
            Choice::Config => match defaults.mode {
                Some(m) => m,
                None => return Ok(None),
            },
        };
        Ok(Some(Options {
            mode,
            score: self.score.filter(|s| *s > 0).unwrap_or(DEFAULT_SCORE),
            limit: self.limit.unwrap_or(defaults.limit),
            harder,
        }))
    }

    /// Reads the rename flags among `args` (`-M[<n>]`, `-C[<n>]`, `--find-renames[=<n>]`, `--find-copies[=<n>]`, `--no-renames`, `-l<n>`).
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut r = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg == "--" {
                break;
            }
            let (kind, value) = if let Some(v) = arg.strip_prefix("--find-renames=") {
                ('M', Some(v))
            } else if let Some(v) = arg.strip_prefix("--find-copies=") {
                ('C', Some(v))
            } else if arg == "--find-renames" {
                ('M', None)
            } else if arg == "--find-copies" {
                ('C', None)
            } else if arg == "--find-copies-harder" {
                r.harder = true;
                continue;
            } else if arg == "--no-renames" {
                r.choice = Choice::Off;
                continue;
            } else if arg.starts_with("-l") && !arg.starts_with("--") {
                let v = match &arg[2..] {
                    "" => args.next().unwrap_or_default(),
                    v => v.to_string(),
                };
                r.limit = Some(
                    v.parse()
                        .map_err(|_| format!("invalid argument to -l: {v}"))?,
                );
                continue;
            } else if let Some(v) = arg.strip_prefix("-M")
                && !arg.starts_with("--")
            {
                ('M', (!v.is_empty()).then_some(v))
            } else if let Some(v) = arg.strip_prefix("-C")
                && !arg.starts_with("--")
            {
                ('C', (!v.is_empty()).then_some(v))
            } else {
                continue;
            };
            r.score = match value.filter(|v| !v.is_empty()) {
                Some(v) => Some(parse_score(v).ok_or_else(|| {
                    let name = if kind == 'M' { "-M" } else { "-C" };
                    format!("invalid argument to {name}: {v}")
                })?),
                None => None,
            };
            if kind == 'C' {
                match r.choice {
                    Choice::On(Mode::Copies) => r.harder = true,
                    Choice::Config => r.over_config = true,
                    _ => {}
                }
            }
            r.choice = Choice::On(if kind == 'M' {
                Mode::Renames
            } else {
                Mode::Copies
            });
        }
        Ok(r)
    }
}

/// Git's `parse_rename_score`: `90%`, `9`, `.5`, with a bare number read as digits after a decimal point.
pub fn parse_score(text: &str) -> Option<u64> {
    let (score, rest) = score_prefix(text);
    rest.is_empty().then_some(score)
}

/// The score `status -M` reads: the leading part of the text, where 0 means the default.
pub fn status_score(text: &str) -> Option<u64> {
    Some(score_prefix(text.strip_prefix('=').unwrap_or(text)).0).filter(|s| *s > 0)
}

fn score_prefix(text: &str) -> (u64, &str) {
    let (mut num, mut scale, mut dot) = (0u64, 1u64, false);
    let mut end = 0;
    for ch in text.chars() {
        if !dot && ch == '.' {
            scale = 1;
            dot = true;
        } else if ch == '%' {
            scale = if dot { scale * 100 } else { 100 };
            end += 1;
            break;
        } else if let Some(d) = ch.to_digit(10) {
            if scale < 100_000 {
                scale *= 10;
                num = num * 10 + u64::from(d);
            }
        } else {
            break;
        }
        end += ch.len_utf8();
    }
    let score = if num >= scale {
        MAX_SCORE
    } else {
        MAX_SCORE * num / scale
    };
    (score, &text[end..])
}

/// A threshold clap checks but does not keep: the value is read from the arguments in order, see `Request::from_args`.
#[derive(Clone, Debug)]
pub struct Checked;

impl std::str::FromStr for Checked {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        if text.is_empty() || parse_score(text).is_some() || text.parse::<i64>().is_ok() {
            Ok(Self)
        } else {
            Err(format!("invalid argument: {text}"))
        }
    }
}

/// The rename switches shared by every command that prints a diff.
#[derive(clap::Args, Debug, Default)]
pub struct Flags {
    /// Detect renames, optionally with a similarity threshold (`-M90%`); on by default in a git repository.
    #[arg(short = 'M', long = "find-renames", value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "", action = clap::ArgAction::Append)]
    find_renames: Vec<Checked>,
    /// Detect copies as well as renames, optionally with a threshold.
    #[arg(short = 'C', long = "find-copies", value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "", action = clap::ArgAction::Append)]
    find_copies: Vec<Checked>,
    /// Turn rename detection off.
    #[arg(long = "no-renames", action = clap::ArgAction::Count)]
    no_renames: u8,
    /// Also take unmodified files as copy sources (`-C -C`).
    #[arg(long = "find-copies-harder", action = clap::ArgAction::Count)]
    find_copies_harder: u8,
    /// Give up exhaustive rename detection beyond this many files on a side (`diff.renameLimit`).
    #[arg(short = 'l', value_name = "N", action = clap::ArgAction::Append)]
    rename_limit: Vec<Checked>,
}

impl Flags {
    /// The request the command line makes; a bad value is a usage error.
    pub fn request(&self) -> Request {
        match Request::from_args(std::env::args()) {
            Ok(r) => r,
            Err(message) => crate::usage_error(&message),
        }
    }
}

/// Rewrites attached `-M90%` and `-C75` to the `=` forms clap takes.
pub fn attached(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let Some(at) = args
        .iter()
        .skip(1)
        .position(|a| {
            ["diff", "show", "log", "history", "stash"]
                .iter()
                .any(|n| a == n)
        })
        .or_else(|| (args.get(1).is_some_and(|a| a == "status")).then_some(0))
        .map(|i| i + 1)
    else {
        return args;
    };
    let mut ended = false;
    args.iter()
        .enumerate()
        .map(|(i, arg)| {
            let text = arg.to_string_lossy();
            ended |= i > at && text == "--";
            if i <= at || ended {
                return arg.clone();
            }
            let attached = |flag: &str, long: &str| {
                text.strip_prefix(flag)
                    .filter(|v| !v.is_empty() && !v.starts_with('-') && !v.starts_with('='))
                    .map(|v| format!("--{long}={v}").into())
            };
            attached("-M", "find-renames")
                .or_else(|| attached("-C", "find-copies"))
                .unwrap_or_else(|| arg.clone())
        })
        .collect()
}

/// A destination path that git reports as moved from `src`.
#[derive(Clone, Debug)]
pub struct Moved {
    pub src: String,
    pub score: u64,
    pub copy: bool,
}

#[derive(Default)]
pub struct Detected {
    /// By destination path.
    pub moved: BTreeMap<String, Moved>,
    /// Deleted paths that a rename or copy accounts for.
    pub consumed: BTreeSet<String>,
}

impl Moved {
    pub fn percent(&self) -> u64 {
        self.score * 100 / MAX_SCORE
    }
}

struct Src<'a> {
    path: &'a str,
    blob: &'a Blob,
    used: u32,
    deleted: bool,
    unmodified: bool,
}

struct Dst<'a> {
    path: &'a str,
    blob: &'a Blob,
    from: Option<(usize, u64)>,
}

#[derive(Clone, Copy)]
struct Score {
    src: usize,
    dst: i64,
    score: i64,
    name: i64,
}

#[derive(Default)]
struct Cached {
    data: Option<Vec<u8>>,
    size: Option<u64>,
    counts: Option<BTreeMap<u32, u64>>,
}

struct Files<'a> {
    backend: &'a dyn Backend,
    cache: HashMap<(bool, usize), Cached>,
}

impl Files<'_> {
    fn load(&mut self, key: (bool, usize), path: &str, blob: &Blob) -> Result<&mut Cached> {
        let backend = self.backend;
        let entry = self.cache.entry(key).or_default();
        if entry.data.is_none() && entry.counts.is_none() {
            let data = blob_bytes(backend, path, blob)?;
            entry.size = Some(data.len() as u64);
            entry.data = Some(data);
        }
        Ok(entry)
    }

    fn size(&mut self, key: (bool, usize), path: &str, blob: &Blob) -> Result<u64> {
        Ok(self.load(key, path, blob)?.size.unwrap_or(0))
    }

    fn counts(&mut self, key: (bool, usize), path: &str, blob: &Blob) -> Result<()> {
        let entry = self.load(key, path, blob)?;
        if entry.counts.is_none() {
            let data = entry.data.take().unwrap_or_default();
            entry.counts = Some(span_counts(&data));
        }
        Ok(())
    }
}

/// Git's span hashes: lines of at most 64 bytes, hashed into `HASHBASE` buckets, with the bytes each bucket covers.
/// A last chunk shorter than a line counts too.
pub(crate) fn span_counts(data: &[u8]) -> BTreeMap<u32, u64> {
    let text = !pyn_diff::is_binary(data);
    let mut counts = BTreeMap::new();
    let (mut a1, mut a2, mut n) = (0u32, 0u32, 0u64);
    let mut add = |a1: u32, a2: u32, n: u64| {
        let hash = a1.wrapping_add(a2.wrapping_mul(0x61)) % HASHBASE;
        *counts.entry(hash).or_insert(0) += n;
    };
    for (i, &c) in data.iter().enumerate() {
        if text && c == b'\r' && data.get(i + 1) == Some(&b'\n') {
            continue;
        }
        let old = a1;
        a1 = (a1 << 7) ^ (a2 >> 25);
        a2 = (a2 << 7) ^ (old >> 25);
        a1 = a1.wrapping_add(u32::from(c));
        n += 1;
        if n < 64 && c != b'\n' {
            continue;
        }
        add(a1, a2, n);
        (a1, a2, n) = (0, 0, 0);
    }
    if n > 0 {
        add(a1, a2, n);
    }
    counts
}

fn basename_same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (a.len(), b.len());
    while i > 0 && j > 0 {
        i -= 1;
        j -= 1;
        if a[i] != b[j] {
            return false;
        }
        if a[i] == b'/' {
            return true;
        }
    }
    (i == 0 || a[i - 1] == b'/') && (j == 0 || b[j - 1] == b'/')
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Git's `estimate_similarity`: how much of `dst` came from `src`, out of `MAX_SCORE`.
fn estimate(
    files: &mut Files,
    src: (usize, &str, &Blob),
    dst: (usize, &str, &Blob),
    minimum: u64,
) -> Result<u64> {
    if !src.2.is_regular() || !dst.2.is_regular() {
        return Ok(0);
    }
    let (s, d) = (
        files.size((true, src.0), src.1, src.2)?,
        files.size((false, dst.0), dst.1, dst.2)?,
    );
    let (max, base_size) = (s.max(d), s.min(d));
    if max * (MAX_SCORE - minimum) < (max - base_size) * MAX_SCORE {
        return Ok(0);
    }
    files.counts((true, src.0), src.1, src.2)?;
    files.counts((false, dst.0), dst.1, dst.2)?;
    let (from, to) = (
        files.cache[&(true, src.0)].counts.as_ref(),
        files.cache[&(false, dst.0)].counts.as_ref(),
    );
    let (from, to) = (from.expect("counted above"), to.expect("counted above"));
    let copied: u64 = from
        .iter()
        .map(|(h, n)| to.get(h).map_or(0, |m| *n.min(m)))
        .sum();
    Ok(if d == 0 { 0 } else { copied * MAX_SCORE / max })
}

fn compare(a: &Score, b: &Score) -> i64 {
    if a.dst < 0 {
        return i64::from(b.dst >= 0);
    }
    if b.dst < 0 {
        return -1;
    }
    if a.score == b.score {
        return b.name - a.name;
    }
    b.score - a.score
}

fn record_if_better(m: &mut [Score], o: Score) {
    let mut worst = 0;
    for i in 1..CANDIDATES {
        if compare(&m[i], &m[worst]) > 0 {
            worst = i;
        }
    }
    if compare(&m[worst], &o) > 0 {
        m[worst] = o;
    }
}

fn exact(srcs: &mut [Src], dsts: &mut [Dst], copies: bool) {
    let mut by_id: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, s) in srcs.iter().enumerate() {
        by_id.entry(s.blob.hash.as_str()).or_default().push(i);
    }
    for dst in dsts.iter_mut() {
        let mut best: Option<(usize, u8)> = None;
        let mut left = 100;
        for &i in by_id.get(dst.blob.hash.as_str()).into_iter().flatten() {
            let s = &srcs[i];
            let regular = s.blob.is_regular() && dst.blob.is_regular();
            if !regular && s.blob.mode != dst.blob.mode {
                continue;
            }
            if s.used > 0 && !copies {
                continue;
            }
            let score = u8::from(s.used == 0) + u8::from(basename_same(s.path, dst.path));
            if best.is_none_or(|(_, b)| score > b) {
                best = Some((i, score));
                if score == 2 {
                    break;
                }
            }
            left -= 1;
            if left == 0 {
                break;
            }
        }
        if let Some((i, _)) = best {
            srcs[i].used += 1;
            dst.from = Some((i, MAX_SCORE));
        }
    }
}

fn record(srcs: &mut [Src], dst: &mut Dst, src: usize, score: u64) {
    srcs[src].used += 1;
    dst.from = Some((src, score));
}

/// Renames (and with `Mode::Copies`, copies) between `old` and `new`, as `git diff -M` / `-C` pairs them.
pub fn detect(backend: &dyn Backend, old: &Tree, new: &Tree, opts: &Options) -> Result<Detected> {
    let copies = opts.mode == Mode::Copies;
    let mut srcs: Vec<Src> = Vec::new();
    let mut dsts: Vec<Dst> = Vec::new();
    let paths: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for path in paths {
        match (old.get(path), new.get(path)) {
            (None, Some(blob)) => dsts.push(Dst {
                path,
                blob,
                from: None,
            }),
            (Some(blob), None) => srcs.push(Src {
                path,
                blob,
                used: 0,
                deleted: true,
                unmodified: false,
            }),
            (Some(a), Some(b))
                if copies
                    && a.is_gitlink() == b.is_gitlink()
                    && (opts.harder || !(a.hash == b.hash && a.mode == b.mode)) =>
            {
                srcs.push(Src {
                    path,
                    blob: a,
                    used: 1,
                    deleted: false,
                    unmodified: a.hash == b.hash && a.mode == b.mode,
                });
            }
            _ => {}
        }
    }
    if !dsts.is_empty() && !srcs.is_empty() {
        pair(backend, &mut srcs, &mut dsts, opts)?;
    }
    let mut uses: Vec<u32> = srcs.iter().map(|s| s.used).collect();
    let mut moved = BTreeMap::new();
    for dst in &dsts {
        if let Some((s, score)) = dst.from {
            uses[s] -= 1;
            moved.insert(
                dst.path.to_string(),
                Moved {
                    src: srcs[s].path.to_string(),
                    score,
                    copy: uses[s] > 0,
                },
            );
        }
    }
    let consumed = srcs
        .iter()
        .filter(|s| s.deleted && s.used > 0)
        .map(|s| s.path.to_string())
        .collect();
    Ok(Detected { moved, consumed })
}

fn pair(backend: &dyn Backend, srcs: &mut [Src], dsts: &mut [Dst], opts: &Options) -> Result<()> {
    let copies = opts.mode == Mode::Copies;
    let minimum = opts.score;
    let mut files = Files {
        backend,
        cache: HashMap::new(),
    };
    exact(srcs, dsts, copies);
    if minimum == MAX_SCORE {
        return Ok(());
    }
    // Indices of the sources still unused by a rename.
    let mut live: Vec<usize> = (0..srcs.len()).collect();
    if !copies {
        let floor = minimum + (MAX_SCORE - minimum) / 2;
        live.retain(|&i| srcs[i].used == 0);
        let mut sources: HashMap<&str, i64> = HashMap::new();
        for &i in &live {
            let b = base(srcs[i].path);
            let v = if sources.contains_key(b) {
                -1
            } else {
                i as i64
            };
            sources.insert(b, v);
        }
        let mut dests: HashMap<&str, i64> = HashMap::new();
        for (i, d) in dsts.iter().enumerate().filter(|(_, d)| d.from.is_none()) {
            let b = base(d.path);
            let v = if dests.contains_key(b) { -1 } else { i as i64 };
            dests.insert(b, v);
        }
        let picks: Vec<(usize, usize)> = live
            .iter()
            .filter_map(|&i| {
                let b = base(srcs[i].path);
                match (sources.get(b), dests.get(b)) {
                    (Some(&s), Some(&d)) if s >= 0 && d >= 0 => Some((i, d as usize)),
                    _ => None,
                }
            })
            .collect();
        for (i, d) in picks {
            if dsts[d].from.is_some() {
                continue;
            }
            let score = estimate(
                &mut files,
                (i, srcs[i].path, srcs[i].blob),
                (d, dsts[d].path, dsts[d].blob),
                floor,
            )?;
            if score >= floor {
                record(srcs, &mut dsts[d], i, score);
            }
        }
        live.retain(|&i| srcs[i].used == 0);
    }
    let remaining = dsts.iter().filter(|d| d.from.is_none()).count();
    if remaining == 0 || live.is_empty() {
        return Ok(());
    }
    let modified = live.iter().filter(|&&i| !srcs[i].unmodified).count();
    match too_many(remaining, (live.len(), modified), opts) {
        Crowd::Fine => {}
        Crowd::Refused => return Ok(()),
        Crowd::Degraded => live.retain(|&i| !srcs[i].unmodified),
    }
    let mut matrix: Vec<Score> = Vec::new();
    for (d, dst) in dsts.iter().enumerate().filter(|(_, d)| d.from.is_none()) {
        let mut m = [Score {
            src: 0,
            dst: -1,
            score: 0,
            name: 0,
        }; CANDIDATES];
        for &j in &live {
            let s = &srcs[j];
            let score = estimate(
                &mut files,
                (j, s.path, s.blob),
                (d, dst.path, dst.blob),
                minimum,
            )?;
            record_if_better(
                &mut m,
                Score {
                    src: j,
                    dst: d as i64,
                    score: score as i64,
                    name: i64::from(basename_same(s.path, dst.path)),
                },
            );
        }
        matrix.extend(m);
    }
    matrix.sort_by(|a, b| compare(a, b).cmp(&0));
    for allow_used in [false, true] {
        if allow_used && !copies {
            break;
        }
        for m in &matrix {
            if m.dst < 0 || (m.score as u64) < minimum {
                break;
            }
            let d = m.dst as usize;
            if dsts[d].from.is_some() || (!allow_used && srcs[m.src].used > 0) {
                continue;
            }
            record(srcs, &mut dsts[d], m.src, m.score as u64);
        }
    }
    Ok(())
}

enum Crowd {
    Fine,
    Refused,
    /// Too many with the unmodified files as sources, not without them.
    Degraded,
}

fn too_many(dsts: usize, (srcs, modified): (usize, usize), opts: &Options) -> Crowd {
    if opts.limit <= 0 {
        return Crowd::Fine;
    }
    let limit = opts.limit as u128;
    let fits = |n: usize| (dsts as u128) * (n as u128) <= limit * limit;
    if fits(srcs) {
        return Crowd::Fine;
    }
    let degraded = opts.harder && fits(modified);
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        if degraded {
            eprintln!("warning: only found copies from modified paths due to too many files.");
        } else {
            eprintln!("warning: exhaustive rename detection was skipped due to too many files.");
        }
        eprintln!(
            "warning: you may want to set your diff.renameLimit variable to at least {} and retry the command.",
            srcs.max(dsts)
        );
    });
    if degraded {
        Crowd::Degraded
    } else {
        Crowd::Refused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(args: &[&str]) -> Request {
        Request::from_args(args.iter().map(|a| a.to_string())).unwrap()
    }

    #[test]
    fn scores_parse_as_git_reads_them() {
        assert_eq!(parse_score("90%"), Some(54_000));
        assert_eq!(parse_score("9"), Some(54_000));
        assert_eq!(parse_score("5"), Some(30_000));
        assert_eq!(parse_score("100"), Some(6_000));
        assert_eq!(parse_score(".5"), Some(30_000));
        assert_eq!(parse_score("100%"), Some(MAX_SCORE));
        assert_eq!(parse_score("9x"), None);
    }

    #[test]
    fn later_flags_win() {
        let r = request(&["diff", "-M", "--no-renames", "-l5"]);
        assert_eq!(r.choice, Choice::Off);
        assert_eq!(r.limit, Some(5));
        let r = request(&["diff", "-M60%", "-C"]);
        assert_eq!(r.score, None);
        let r = request(&["diff", "--no-renames", "-C75%"]);
        assert_eq!(r.choice, Choice::On(Mode::Copies));
        assert_eq!(r.score, Some(45_000));
        assert!(request(&["diff", "-C", "-C"]).harder);
        assert!(!request(&["diff", "-C", "-M", "-C"]).harder);
        assert!(!request(&["diff", "--no-renames", "-C"]).harder);
        let r = request(&["diff", "--no-renames", "--find-copies-harder"]);
        assert!(r.resolve(Defaults::default()).unwrap().unwrap().harder);
        let copies = Defaults {
            mode: Some(Mode::Copies),
            ..Defaults::default()
        };
        assert!(
            request(&["diff", "-C"])
                .resolve(copies)
                .unwrap()
                .unwrap()
                .harder
        );
        assert!(
            !request(&["diff", "-M", "-C"])
                .resolve(copies)
                .unwrap()
                .unwrap()
                .harder
        );
    }
}

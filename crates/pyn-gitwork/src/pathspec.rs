use std::sync::Arc;

use crate::attributes::{AttrValue, Attributes};
use crate::wildmatch::matches_fold;

/// Git's pathspecs: literal paths and folders, globs and the `:(...)` magic, matched on repository paths
/// (`/`-separated, from the root). An empty pathspec matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pathspec {
    items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    /// The path from the root, prefix included; a trailing `/` is kept.
    text: Vec<u8>,
    /// The argument as given, for messages.
    original: Vec<u8>,
    /// Leading bytes of `text` that are literal whatever they contain (the working folder, then the text up to the
    /// first wildcard).
    nowild: usize,
    /// Bytes of `text` that come from the working folder.
    prefix: usize,
    magic: MagicSet,
    attr: Option<AttrSpec>,
}

/// What one `attr:` entry asks of an attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AttrWant {
    Set,
    Unset,
    Unspecified,
    Value(Vec<u8>),
}

/// The `:(attr:...)` requirements of one pathspec, with the attributes they are checked against.
#[derive(Debug, Clone)]
struct AttrSpec {
    wants: Vec<(String, AttrWant)>,
    source: Arc<Attributes>,
}

impl PartialEq for AttrSpec {
    fn eq(&self, other: &Self) -> bool {
        self.wants == other.wants && Arc::ptr_eq(&self.source, &other.source)
    }
}

impl Eq for AttrSpec {}

impl AttrSpec {
    fn text(&self) -> String {
        let word = |(name, want): &(String, AttrWant)| match want {
            AttrWant::Set => name.clone(),
            AttrWant::Unset => format!("-{name}"),
            AttrWant::Unspecified => format!("!{name}"),
            AttrWant::Value(v) => format!("{name}={}", lossy(v)),
        };
        self.wants.iter().map(word).collect::<Vec<_>>().join(" ")
    }

    fn matches(&self, name: &[u8]) -> bool {
        let name = name.strip_suffix(b"/").unwrap_or(name);
        let have = self.source.for_path(name);
        self.wants
            .iter()
            .all(|(attr, want)| match (have.get(attr), want) {
                (AttrValue::Set, AttrWant::Set)
                | (AttrValue::Unset, AttrWant::Unset)
                | (AttrValue::Unspecified, AttrWant::Unspecified) => true,
                (AttrValue::Value(v), AttrWant::Value(w)) => v == *w,
                _ => false,
            })
    }
}

/// The pathspec magic in effect for one pathspec, or the kinds a command refuses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MagicSet {
    pub top: bool,
    pub literal: bool,
    pub glob: bool,
    pub icase: bool,
    pub exclude: bool,
    pub attr: bool,
}

impl MagicSet {
    fn refused(self, by: Self) -> Vec<&'static str> {
        [
            (self.top && by.top, "'top' (mnemonic: '/')"),
            (self.literal && by.literal, "'literal'"),
            (self.glob && by.glob, "'glob'"),
            (self.icase && by.icase, "'icase'"),
            (self.exclude && by.exclude, "'exclude' (mnemonic: '!')"),
            (self.attr && by.attr, "'attr'"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect()
    }
}

/// What git's directory walk does with an untracked folder given the pathspecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirMatch {
    /// Not selected: skip it.
    None,
    /// Something below may be selected: look inside.
    Leading,
    /// Selected as a whole: it is one entry when it holds a selected file.
    Whole,
}

#[derive(Debug, Clone, Copy, Default)]
struct Mode {
    directory: bool,
    leading: bool,
    /// Skip the `attr:` requirements, as git does for a folder it only lists whole.
    no_attr: bool,
}

/// How a pathspec selected a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Match {
    /// The path is under the named folder.
    Recursive,
    /// The folder may hold matches (only asked for with a folder).
    Leading,
    Fnmatch,
    /// The path is the one named.
    Exact,
}

/// The `GIT_*_PATHSPECS` settings, which apply to every pathspec of a command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PathspecOptions {
    pub literal: bool,
    pub glob: bool,
    pub noglob: bool,
    pub icase: bool,
}

/// What [`Pathspec::parse`] needs to know about the command line.
#[derive(Debug, Clone, Copy)]
pub struct PathspecContext<'a> {
    /// The work tree's absolute path, to resolve absolute arguments.
    pub root: &'a [u8],
    /// The working folder from the root, without a trailing slash.
    pub prefix: &'a [u8],
    pub options: PathspecOptions,
    /// Magic the command refuses, as git does for `add` (`attr`) and `ls-tree` (all but `top` and `literal`).
    pub unsupported: MagicSet,
    /// With no argument, or only excludes, limit to the working folder rather than the whole repository.
    pub prefer_cwd: bool,
    /// The attributes `:(attr:...)` is checked against; without them that magic is unsupported.
    pub attributes: Option<&'a Arc<Attributes>>,
}

/// Why a pathspec failed to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathspecError {
    /// Git dies with this message (status 128).
    Fatal(String),
    /// Valid in git but not built here; the caller reports a usage error.
    Unsupported(String),
}

impl std::fmt::Display for PathspecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Fatal(m) | Self::Unsupported(m)) = self;
        f.write_str(m)
    }
}

impl std::error::Error for PathspecError {}

const GLOB_CHARS: &[u8] = b"*?[\\";

fn simple_length(p: &[u8]) -> usize {
    p.iter()
        .position(|b| GLOB_CHARS.contains(b))
        .unwrap_or(p.len())
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Pathspec {
    /// Literal paths and folders; a trailing slash is dropped.
    pub fn new(paths: impl IntoIterator<Item = impl Into<Vec<u8>>>) -> Self {
        let items = paths
            .into_iter()
            .map(Into::into)
            .map(|p: Vec<u8>| trim_slashes(&p).to_vec())
            .filter(|p| !p.is_empty())
            .map(|p| Item {
                nowild: p.len(),
                original: p.clone(),
                text: p,
                prefix: 0,
                magic: MagicSet::default(),
                attr: None,
            })
            .collect();
        Self { items }
    }

    /// Parses command-line pathspecs as git does: magic, relative paths, the global settings.
    pub fn parse(
        args: &[impl AsRef<[u8]>],
        cx: &PathspecContext<'_>,
    ) -> Result<Self, PathspecError> {
        let mut items = Vec::new();
        for arg in args {
            items.push(parse_item(arg.as_ref(), cx)?);
        }
        let excludes = items.iter().filter(|i| i.magic.exclude).count();
        if items.len() == excludes && (!items.is_empty() || cx.prefer_cwd) {
            let text = if cx.prefer_cwd && !cx.prefix.is_empty() {
                [cx.prefix, b"/"].concat()
            } else {
                Vec::new()
            };
            if !(items.is_empty() && text.is_empty()) {
                items.push(Item {
                    nowild: text.len(),
                    prefix: text.len(),
                    original: Vec::new(),
                    text,
                    magic: MagicSet::default(),
                    attr: None,
                });
            }
        }
        Ok(Self { items })
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many pathspecs there are, excludes included.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The `n`th pathspec as given on the command line.
    pub fn original(&self, n: usize) -> &[u8] {
        &self.items[n].original
    }

    /// Picks from `paths`, in order, the ones this selects; see [`Tracker`] for how that differs from `matches`.
    pub fn select<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a [u8]>,
    ) -> (Vec<&'a [u8]>, Vec<Option<Match>>) {
        let mut tracker = self.tracker();
        let picked = paths.into_iter().filter(|p| tracker.picks(p)).collect();
        (picked, tracker.seen)
    }

    /// For each pathspec, the strongest way it selected one of `paths`; an exclude is never reported.
    pub fn seen<'a>(&self, paths: impl IntoIterator<Item = &'a [u8]>) -> Vec<Option<Match>> {
        self.select(paths).1
    }

    pub fn tracker(&self) -> Tracker<'_> {
        Tracker {
            spec: self,
            seen: vec![None; self.items.len()],
        }
    }

    /// The pathspecs, by position, that select none of `paths` (git's `report_path_error`); a repeat of one that did
    /// select something is not reported.
    pub fn unmatched<'a>(&self, paths: impl IntoIterator<Item = &'a [u8]>) -> Vec<usize> {
        let seen = self.seen(paths);
        (0..self.items.len())
            .filter(|n| seen[*n].is_none() && !self.items[*n].magic.exclude)
            .filter(|n| {
                !(0..self.items.len())
                    .any(|m| seen[m].is_some() && self.items[m].original == self.items[*n].original)
            })
            .collect()
    }

    /// The first of [`Self::unmatched`], as given.
    pub fn first_unmatched<'a>(&self, paths: impl IntoIterator<Item = &'a [u8]>) -> Option<&[u8]> {
        let n = *self.unmatched(paths).first()?;
        Some(&self.items[n].original)
    }

    /// How the `n`th pathspec selects `path`, if it does (never for an exclude).
    pub fn item_match(&self, n: usize, path: &[u8]) -> Option<Match> {
        let item = &self.items[n];
        item.how(path, Mode::default())
            .filter(|_| !item.magic.exclude)
    }

    /// The `n`th pathspec's path from the root, a trailing slash kept.
    pub fn item_path(&self, n: usize) -> &[u8] {
        &self.items[n].text
    }

    /// Whether the `n`th pathspec asks for attributes.
    pub fn item_has_attr(&self, n: usize) -> bool {
        self.items[n].attr.is_some()
    }

    /// Whether the `n`th pathspec is an exclude.
    pub fn item_is_exclude(&self, n: usize) -> bool {
        self.items[n].magic.exclude
    }

    /// Whether the `n`th pathspec is read as a glob or ignoring case, so naming a file does not make it match.
    pub fn item_is_pattern(&self, n: usize) -> bool {
        self.items[n].magic.glob || self.items[n].magic.icase
    }

    /// Whether the `n`th pathspec is a `:(glob)` pattern ending in a slash, which names folders by pattern.
    pub fn item_is_glob_folder(&self, n: usize) -> bool {
        let i = &self.items[n];
        i.magic.glob && i.nowild < i.text.len() && i.text.ends_with(b"/")
    }

    /// The `n`th pathspec as git words it in `stash` messages: its magic, its working-folder length and its path.
    pub fn item_form(&self, n: usize) -> String {
        let i = &self.items[n];
        let mut parts: Vec<String> = [
            (i.magic.top, "top"),
            (i.magic.literal, "literal"),
            (i.magic.glob, "glob"),
            (i.magic.icase, "icase"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| name.to_string())
        .collect();
        if let Some(attr) = &i.attr {
            parts.push(format!("attr:{}", attr.text()));
        }
        parts.push(format!("prefix:{}", i.prefix));
        format!(":({}){}", parts.join(","), lossy(&i.text))
    }

    /// The `n`th pathspec's text up to its first wildcard.
    pub fn item_literal(&self, n: usize) -> &[u8] {
        let i = &self.items[n];
        &i.text[..i.nowild]
    }

    pub fn matches(&self, path: &[u8]) -> bool {
        self.chosen(|i| i.how(path, Mode::default()).is_some())
    }

    /// Whether the folder `path` (no trailing slash) is selected as a whole, which git decides by matching `path/`
    /// (so `sub/*` takes `sub`, and `:(glob)su*` does not).
    pub fn matches_dir(&self, path: &[u8]) -> bool {
        self.dir_match(path) == DirMatch::Whole
    }

    /// How an untracked folder `path` (no trailing slash) stands with the pathspecs, as git's directory walk sees it.
    pub fn dir_match(&self, path: &[u8]) -> DirMatch {
        if self.items.is_empty() {
            return DirMatch::Whole;
        }
        let slashed = [path, b"/"].concat();
        let leading = Mode {
            directory: false,
            leading: true,
            no_attr: false,
        };
        let positive = self
            .items
            .iter()
            .filter(|i| !i.magic.exclude)
            .filter_map(|i| i.how(&slashed, leading))
            .max();
        let excluded = self
            .items
            .iter()
            .any(|i| i.magic.exclude && i.how(&slashed, Mode::default()).is_some());
        match positive {
            None => DirMatch::None,
            _ if excluded => DirMatch::None,
            Some(Match::Leading) => DirMatch::Leading,
            Some(_) => DirMatch::Whole,
        }
    }

    fn chosen(&self, hit: impl Fn(&Item) -> bool) -> bool {
        self.items.is_empty()
            || (self.items.iter().any(|i| !i.magic.exclude && hit(i))
                && !self.items.iter().any(|i| i.magic.exclude && hit(i)))
    }

    /// Whether a pattern could match something under the folder, judging only by the literal text before its first
    /// wildcard (so `*.log` leads into every folder, `u*` only into folders starting with `u`).
    pub fn glob_leads_into(&self, dir: &[u8]) -> bool {
        let slashed = [dir, b"/"].concat();
        let blind = Mode {
            leading: true,
            no_attr: true,
            ..Mode::default()
        };
        self.items.iter().any(|i| {
            !i.magic.exclude
                && ((i.nowild < i.text.len() && {
                    let literal = &i.text[..i.nowild];
                    dir.starts_with(literal) || is_under(literal, dir)
                }) || (i.attr.is_some() && i.how(&slashed, blind).is_some()))
        })
    }

    /// Whether a walk of untracked files goes into the folder `dir` (no trailing slash), which git decides by
    /// matching `dir/`, so an exclude that takes the folder keeps the walk out.
    pub fn walks_into(&self, dir: &[u8]) -> bool {
        self.dir_match(dir) != DirMatch::None
    }

    /// Whether anything under the folder `dir` (no trailing slash) can match.
    pub fn may_contain(&self, dir: &[u8]) -> bool {
        let folder = Mode {
            directory: true,
            leading: true,
            no_attr: false,
        };
        self.items.is_empty()
            || self
                .items
                .iter()
                .any(|i| !i.magic.exclude && i.how(dir, folder).is_some())
    }
}

/// Matching as git does it for the commands that report unmatched pathspecs: a literal pathspec that has named a path
/// exactly selects nothing after it (git before 2.50 did so for patterns too).
#[derive(Debug)]
pub struct Tracker<'a> {
    spec: &'a Pathspec,
    seen: Vec<Option<Match>>,
}

impl Tracker<'_> {
    pub fn picks(&mut self, path: &[u8]) -> bool {
        let items = &self.spec.items;
        if items.is_empty() {
            return true;
        }
        let mut best = None;
        for (n, item) in items
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, i)| !i.magic.exclude)
        {
            if self.seen[n] == Some(Match::Exact) && item.nowild == item.text.len() {
                continue;
            }
            if let Some(how) = item.how(path, Mode::default()) {
                best = best.max(Some(how));
                self.seen[n] = self.seen[n].max(Some(how));
            }
        }
        best.is_some()
            && !items
                .iter()
                .any(|i| i.magic.exclude && i.how(path, Mode::default()).is_some())
    }
}

impl Item {
    fn same(&self, a: &[u8], b: &[u8]) -> bool {
        if self.magic.icase {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    }

    fn how(&self, name: &[u8], mode: Mode) -> Option<Match> {
        if !(mode.directory || mode.no_attr) && self.attr.as_ref().is_some_and(|a| !a.matches(name))
        {
            return None;
        }
        let m = &self.text[..];
        if self.prefix > 0 && self.magic.icase && !self.in_prefix(name, mode) {
            return None;
        }
        if m.is_empty() {
            return Some(Match::Recursive);
        }
        if m.len() <= name.len() && self.same(m, &name[..m.len()]) {
            if m.len() == name.len() {
                return Some(Match::Exact);
            }
            if m[m.len() - 1] == b'/' || name[m.len()] == b'/' {
                return Some(Match::Recursive);
            }
        } else if mode.directory
            && m[m.len() - 1] == b'/'
            && name.len() + 1 == m.len()
            && self.same(&m[..name.len()], name)
        {
            return Some(Match::Exact);
        }
        if self.nowild < m.len() && self.fnmatch(name) {
            return Some(Match::Fnmatch);
        }
        if !mode.leading {
            return None;
        }
        let offset = usize::from(name.last() == Some(&b'/'));
        if name.len() < m.len()
            && !name.is_empty()
            && m[name.len() - offset] == b'/'
            && self.same(&m[..name.len()], name)
        {
            return Some(Match::Leading);
        }
        let n = self.nowild;
        if n < m.len() && !(name.len() >= n && self.same(&m[..n], &name[..n])) {
            return None;
        }
        (n < m.len()).then_some(Match::Leading)
    }

    /// Under `:(icase)` the working folder part of the name must match exactly (a folder may be named without its slash).
    fn in_prefix(&self, name: &[u8], mode: Mode) -> bool {
        let (m, prefix) = (&self.text[..], self.prefix);
        if name.len() >= prefix {
            name[..prefix] == m[..prefix]
        } else {
            mode.directory && m[..name.len()] == *name && m[name.len()] == b'/'
        }
    }

    fn fnmatch(&self, name: &[u8]) -> bool {
        let n = self.nowild;
        name.len() >= n
            && self.same(&self.text[..n], &name[..n])
            && matches_fold(
                &self.text[n..],
                &name[n..],
                self.magic.glob,
                self.magic.icase,
            )
    }
}

fn parse_item(arg: &[u8], cx: &PathspecContext<'_>) -> Result<Item, PathspecError> {
    let fatal = |m: String| PathspecError::Fatal(m);
    if arg.is_empty() {
        return Err(fatal(
            "empty string is not a valid pathspec. please use . instead if you meant to match all paths"
                .into(),
        ));
    }
    let (mut magic, wants, rest) = if arg[0] == b':' && !cx.options.literal {
        parse_magic(arg)?
    } else {
        (MagicSet::default(), None, arg)
    };
    let opts = cx.options;
    let mut global = MagicSet {
        literal: opts.literal,
        glob: opts.glob && !magic.literal,
        icase: opts.icase,
        ..MagicSet::default()
    };
    if opts.glob && opts.noglob {
        return Err(fatal(
            "global 'glob' and 'noglob' pathspec settings are incompatible".into(),
        ));
    }
    if global.literal && (global.glob || global.icase) {
        return Err(fatal(
            "global 'literal' pathspec setting is incompatible with all other global pathspec settings"
                .into(),
        ));
    }
    global.literal |= opts.noglob && !magic.glob;
    magic.literal |= global.literal;
    magic.glob |= global.glob;
    magic.icase |= global.icase;
    if magic.literal && magic.glob {
        return Err(fatal(format!(
            "{}: 'literal' and 'glob' are incompatible",
            lossy(arg)
        )));
    }
    let (text, prefix) = if magic.top {
        (rest.to_vec(), 0)
    } else {
        rooted(arg, rest, cx)?
    };
    let refused = magic.refused(cx.unsupported);
    if !refused.is_empty() {
        return Err(fatal(format!(
            "{}: pathspec magic not supported by this command: {}",
            lossy(arg),
            refused.join(", ")
        )));
    }
    let attr = match (wants, cx.attributes) {
        (Some(wants), Some(source)) => Some(AttrSpec {
            wants,
            source: source.clone(),
        }),
        (_, None) if magic.attr => {
            return Err(PathspecError::Unsupported(
                "pathspec magic 'attr' is not supported".into(),
            ));
        }
        _ => None,
    };
    let nowild = if magic.literal {
        text.len()
    } else {
        simple_length(&text).max(prefix)
    };
    Ok(Item {
        text,
        original: arg.to_vec(),
        nowild,
        prefix,
        magic,
        attr,
    })
}

type Wants = Vec<(String, AttrWant)>;

/// Reads the `:` magic at the start of `arg`; returns it, the `attr:` requirements and the path after it.
fn parse_magic(arg: &[u8]) -> Result<(MagicSet, Option<Wants>, &[u8]), PathspecError> {
    let fatal = |m: String| PathspecError::Fatal(m);
    let mut magic = MagicSet::default();
    if arg.get(1) != Some(&b'(') {
        let mut at = 1;
        while at < arg.len() && arg[at] != b':' {
            match arg[at] {
                b'/' => magic.top = true,
                b'!' | b'^' => magic.exclude = true,
                c if b"\"#%&',-;<=>@_`~".contains(&c) => {
                    return Err(fatal(format!(
                        "Unimplemented pathspec magic '{}' in '{}'",
                        c as char,
                        lossy(arg)
                    )));
                }
                _ => break,
            }
            at += 1;
        }
        let rest = &arg[at + usize::from(arg.get(at) == Some(&b':'))..];
        return Ok((magic, None, rest));
    }
    let mut at = 2;
    let mut wants = None;
    while at < arg.len() && arg[at] != b')' {
        let len = if arg[at..].starts_with(b"attr:") {
            attr_body_len(&arg[at..])?
        } else {
            arg[at..]
                .iter()
                .position(|b| *b == b',' || *b == b')')
                .unwrap_or(arg.len() - at)
        };
        let name = &arg[at..at + len];
        at += len + usize::from(arg.get(at + len) == Some(&b','));
        match name {
            b"" => {}
            b"top" => magic.top = true,
            b"literal" => magic.literal = true,
            b"glob" => magic.glob = true,
            b"icase" => magic.icase = true,
            b"exclude" => magic.exclude = true,
            b"attr" => magic.attr = true,
            n if n.starts_with(b"attr:") => {
                if wants.is_some() {
                    return Err(fatal("Only one 'attr:' specification is allowed.".into()));
                }
                magic.attr = true;
                wants = Some(parse_attr_body(&n[5..])?);
            }
            n if n.starts_with(b"prefix:") => {
                return Err(PathspecError::Unsupported(
                    "pathspec magic 'prefix' is not supported".into(),
                ));
            }
            n => {
                return Err(fatal(format!(
                    "Invalid pathspec magic '{}' in '{}'",
                    lossy(n),
                    lossy(arg)
                )));
            }
        }
    }
    if arg.get(at) != Some(&b')') {
        return Err(fatal(format!(
            "Missing ')' at the end of pathspec magic in '{}'",
            lossy(arg)
        )));
    }
    Ok((magic, wants, &arg[at + 1..]))
}

/// The length of an `attr:` entry, which runs to the next `,` or `)` that is not backslash-escaped.
fn attr_body_len(from: &[u8]) -> Result<usize, PathspecError> {
    let mut i = 0;
    while i < from.len() && from[i] != b',' && from[i] != b')' {
        if from[i] == b'\\' {
            if i + 1 == from.len() {
                return Err(escape_last());
            }
            i += 1;
        }
        i += 1;
    }
    Ok(i)
}

fn escape_last() -> PathspecError {
    PathspecError::Fatal("Escape character '\\' not allowed as last character in attr value".into())
}

fn attr_name_valid(name: &[u8]) -> bool {
    !name.is_empty()
        && name[0] != b'-'
        && name
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(b))
}

fn parse_attr_body(body: &[u8]) -> Result<Wants, PathspecError> {
    let fatal = |m: String| PathspecError::Fatal(m);
    let words: Vec<&[u8]> = body
        .split(|b| *b == b' ')
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return Err(fatal("attr spec must not be empty".into()));
    }
    let mut wants = Vec::new();
    for word in words {
        let (name, want) = match word[0] {
            b'!' => (&word[1..], AttrWant::Unspecified),
            b'-' => (&word[1..], AttrWant::Unset),
            _ => match word.iter().position(|b| *b == b'=') {
                Some(eq) => (&word[..eq], AttrWant::Value(attr_value(&word[eq + 1..])?)),
                None => (word, AttrWant::Set),
            },
        };
        if !attr_name_valid(name) {
            return Err(fatal(format!("invalid attribute name {}", lossy(name))));
        }
        wants.push((lossy(name), want));
    }
    Ok(wants)
}

/// Unescapes a matched attribute value; only letters, digits, `,`, `-` and `_` may remain.
fn attr_value(raw: &[u8]) -> Result<Vec<u8>, PathspecError> {
    let mut out = Vec::new();
    let mut bytes = raw.iter();
    while let Some(&b) = bytes.next() {
        let b = if b == b'\\' {
            *bytes.next().ok_or_else(escape_last)?
        } else {
            b
        };
        if !(b.is_ascii_alphanumeric() || b"-_,".contains(&b)) {
            return Err(PathspecError::Fatal(format!(
                "cannot use '{}' for value matching",
                lossy(&[b])
            )));
        }
        out.push(b);
    }
    Ok(out)
}

/// The path from the root for `rest` as typed in the working folder, and how much of it is that folder.
fn rooted(
    arg: &[u8],
    rest: &[u8],
    cx: &PathspecContext<'_>,
) -> Result<(Vec<u8>, usize), PathspecError> {
    let outside = || {
        PathspecError::Fatal(format!(
            "{}: '{}' is outside repository at '{}'",
            lossy(arg),
            lossy(rest),
            lossy(cx.root)
        ))
    };
    let segments = |p: &'_ [u8]| -> Vec<Vec<u8>> {
        p.split(|b| *b == b'/')
            .filter(|s| !s.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    };
    let absolute = rest.starts_with(b"/");
    let mut out = if absolute {
        Vec::new()
    } else {
        segments(cx.prefix)
    };
    let mut kept = out.len();
    let mut trailing = rest.is_empty() || rest.ends_with(b"/");
    for (n, seg) in rest.split(|b| *b == b'/').enumerate() {
        let last = rest.split(|b| *b == b'/').count() == n + 1;
        match seg {
            b"" => {}
            b"." => trailing |= last,
            b".." => {
                out.pop().ok_or_else(outside)?;
                kept = kept.min(out.len());
                trailing |= last;
            }
            s => out.push(s.to_vec()),
        }
    }
    if absolute {
        let root = segments(cx.root);
        if !out.starts_with(&root) {
            return Err(outside());
        }
        out.drain(..root.len());
        kept = 0;
    }
    let prefix_len = out.iter().take(kept).map(|s| s.len() + 1).sum();
    let mut text = out.join(&b'/');
    if trailing && !text.is_empty() {
        text.push(b'/');
    }
    Ok((text, prefix_len))
}

/// The pathspecs in a `--pathspec-from-file` file: one per line (a line may be C-quoted, and may end in CR), or with
/// `nul` separated by NUL and taken as written. Git leaves the empty-string check to the pathspec parser.
pub fn read_pathspec_file(data: &[u8], nul: bool) -> Result<Vec<Vec<u8>>, PathspecError> {
    let sep = if nul { 0 } else { b'\n' };
    let mut lines: Vec<&[u8]> = data.split(|b| *b == sep).collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|line| {
            if nul {
                return Ok(line.to_vec());
            }
            if line.first() == Some(&b'"') {
                return crate::attributes::unquote(line)
                    .map(|(name, _)| name)
                    .ok_or_else(|| {
                        PathspecError::Fatal(format!("line is badly quoted: {}", lossy(line)))
                    });
            }
            Ok(line.strip_suffix(b"\r").unwrap_or(line).to_vec())
        })
        .collect()
}

fn trim_slashes(p: &[u8]) -> &[u8] {
    let mut p = p;
    while let Some(rest) = p.strip_suffix(b"/") {
        p = rest;
    }
    p
}

fn is_under(path: &[u8], prefix: &[u8]) -> bool {
    path == prefix || (path.starts_with(prefix) && path.get(prefix.len()) == Some(&b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_in(args: &[&str], prefix: &str) -> Result<Pathspec, PathspecError> {
        let cx = PathspecContext {
            root: b"/work",
            prefix: prefix.as_bytes(),
            options: PathspecOptions::default(),
            unsupported: MagicSet::default(),
            prefer_cwd: false,
            attributes: None,
        };
        Pathspec::parse(args, &cx)
    }

    fn spec(args: &[&str]) -> Pathspec {
        parse_in(args, "").unwrap()
    }

    #[test]
    fn pathspec_files_split_on_lines_or_nul() {
        let read = |d: &[u8], nul| read_pathspec_file(d, nul).unwrap();
        assert_eq!(read(b"a\r\nb", false), [b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(
            read(b"\"s p\\303\\251\"x\n", false),
            ["s p\u{e9}".as_bytes()]
        );
        assert_eq!(
            read(b"\"a\"\0b c\0", true),
            [b"\"a\"".to_vec(), b"b c".to_vec()]
        );
        assert_eq!(read(b"a\n\n", false), [b"a".to_vec(), Vec::new()]);
        assert!(read(b"", false).is_empty());
        assert!(read_pathspec_file(b"\"a\n", false).is_err());
    }

    #[test]
    fn attr_magic_selects_by_attribute_state() {
        let attrs = Arc::new(Attributes::new(
            b"",
            b"",
            Box::new(|dir: &[u8]| {
                dir.is_empty()
                    .then(|| b"*.c foo\n*.h -foo v=a,b\n".to_vec())
            }),
            false,
        ));
        let with = |arg: &str| {
            let cx = PathspecContext {
                root: b"/work",
                prefix: b"",
                options: PathspecOptions::default(),
                unsupported: MagicSet::default(),
                prefer_cwd: false,
                attributes: Some(&attrs),
            };
            Pathspec::parse(&[arg], &cx)
        };
        let m = |arg: &str, path: &[u8]| with(arg).unwrap().matches(path);
        assert!(m(":(attr:foo)", b"x.c") && !m(":(attr:foo)", b"x.h") && !m(":(attr:foo)", b"x.t"));
        assert!(m(":(attr:-foo)", b"x.h") && m(":(attr:!foo)", b"x.t"));
        assert!(m(":(attr:v=a\\,b)", b"x.h") && !m(":(attr:v=a)", b"x.h"));
        let fatal = |arg: &str| match with(arg) {
            Err(PathspecError::Fatal(m)) => m,
            other => panic!("{other:?}"),
        };
        assert_eq!(fatal(":(attr:)"), "attr spec must not be empty");
        assert_eq!(
            fatal(":(attr:a,attr:b)"),
            "Only one 'attr:' specification is allowed."
        );
        assert_eq!(fatal(":(attr:-a=b)"), "invalid attribute name a=b");
        assert_eq!(fatal(":(attr:v=a.b)"), "cannot use '.' for value matching");
    }

    #[test]
    fn matches_files_and_folders_but_not_name_prefixes() {
        let s = Pathspec::new(["src", "a.txt/"]);
        assert!(s.matches(b"src/x.rs") && s.matches(b"src") && s.matches(b"a.txt"));
        assert!(!s.matches(b"srcx/y") && !s.matches(b"a.txt.bak"));
        assert!(Pathspec::default().matches(b"anything"));
    }

    #[test]
    fn a_trailing_slash_names_a_folder_only() {
        let s = spec(&["a.txt/", "src/"]);
        assert!(!s.matches(b"a.txt") && s.matches(b"src/a.rs") && !s.matches(b"src"));
        assert!(s.matches_dir(b"src") && s.matches_dir(b"a.txt") && !s.matches_dir(b"zzz"));
    }

    #[test]
    fn plain_wildcards_cross_folders_and_glob_magic_does_not() {
        let s = spec(&["*.txt", "src/a*"]);
        assert!(s.matches(b"x.txt") && s.matches(b"deep/x.txt") && s.matches(b"src/abc/y"));
        assert!(!s.matches(b"x.log") && !s.matches(b"src/b"));
        let g = spec(&[":(glob)src/*.rs", ":(glob)**/x.go"]);
        assert!(g.matches(b"src/a.rs") && !g.matches(b"src/deep/a.rs"));
        assert!(g.matches(b"x.go") && g.matches(b"a/b/x.go"));
        assert!(!spec(&[":(glob)src*"]).matches(b"src/a"));
        assert!(
            spec(&[":(glob)src**"]).matches(b"src/a"),
            "git reads `**` here as a whole segment"
        );
    }

    #[test]
    fn literal_magic_and_a_literal_name_beat_the_wildcards() {
        let s = spec(&[":(literal)a*c"]);
        assert!(s.matches(b"a*c") && !s.matches(b"abc"));
        assert!(spec(&["a*c"]).matches(b"a*c") && spec(&["a*c"]).matches(b"abc"));
        assert!(spec(&[r"a\*c"]).matches(b"a*c") && !spec(&[r"a\*c"]).matches(b"abc"));
    }

    #[test]
    fn icase_folds_ascii_letters() {
        let s = spec(&[":(icase)SRC/*.RS"]);
        assert!(s.matches(b"src/a.rs") && s.matches(b"Src/B.Rs"));
        assert!(
            !spec(&[":(icase)src/[A]*"]).matches(b"src/a"),
            "git keeps bracket members as written"
        );
        assert!(spec(&[":(icase)[A-Z]rc"]).matches(b"src"));
    }

    #[test]
    fn excludes_remove_from_the_rest_and_alone_leave_everything_else() {
        let s = spec(&["src", ":!src/deep", ":(exclude)*.md"]);
        assert!(s.matches(b"src/a.rs") && !s.matches(b"src/deep/x") && !s.matches(b"src/r.md"));
        let alone = spec(&[":^docs"]);
        assert!(alone.matches(b"src/a") && !alone.matches(b"docs/a"));
        let cwd = Pathspec::parse(
            &[":!x"],
            &PathspecContext {
                root: b"/work",
                prefix: b"src",
                options: PathspecOptions::default(),
                unsupported: MagicSet::default(),
                prefer_cwd: true,
                attributes: None,
            },
        )
        .unwrap();
        assert!(cwd.matches(b"src/a") && !cwd.matches(b"other/a") && !cwd.matches(b"src/x"));
    }

    #[test]
    fn relative_paths_resolve_against_the_working_folder() {
        let s = parse_in(&["../docs", "./a.txt", "sub/../b"], "src").unwrap();
        assert_eq!(s.item_path(0), b"docs");
        assert_eq!(s.item_path(1), b"src/a.txt");
        assert_eq!(s.item_path(2), b"src/b");
        assert_eq!(parse_in(&[":/top"], "src").unwrap().item_path(0), b"top");
        assert_eq!(parse_in(&["."], "src").unwrap().item_path(0), b"src/");
        assert_eq!(parse_in(&["x/.."], "").unwrap().item_path(0), b"");
        assert_eq!(parse_in(&["a/b/.."], "").unwrap().item_path(0), b"a/");
        assert_eq!(
            parse_in(&["/work/x//y/"], "src").unwrap().item_path(0),
            b"x/y/"
        );
    }

    #[test]
    fn the_working_folder_is_never_a_pattern() {
        let s = parse_in(&["*.rs"], "g*b").unwrap();
        assert!(s.matches(b"g*b/x.rs") && !s.matches(b"gxb/x.rs"));
    }

    #[test]
    fn bad_pathspecs_fail_with_gits_words() {
        let fatal = |args: &[&str]| match parse_in(args, "") {
            Err(PathspecError::Fatal(m)) => m,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            fatal(&[":(bogus)x"]),
            "Invalid pathspec magic 'bogus' in ':(bogus)x'"
        );
        assert_eq!(
            fatal(&[":(top"]),
            "Missing ')' at the end of pathspec magic in ':(top'"
        );
        assert_eq!(fatal(&[":#x"]), "Unimplemented pathspec magic '#' in ':#x'");
        assert_eq!(
            fatal(&[":(glob,literal)x"]),
            ":(glob,literal)x: 'literal' and 'glob' are incompatible"
        );
        assert_eq!(
            fatal(&["../x"]),
            "../x: '../x' is outside repository at '/work'"
        );
        assert_eq!(
            fatal(&["/etc/x"]),
            "/etc/x: '/etc/x' is outside repository at '/work'"
        );
        assert!(fatal(&[""]).starts_with("empty string is not a valid pathspec"));
        assert!(matches!(
            parse_in(&[":(attr:x)y"], ""),
            Err(PathspecError::Unsupported(_))
        ));
    }

    #[test]
    fn global_settings_apply_to_every_pathspec() {
        let with = |options: PathspecOptions, arg: &str| {
            let cx = PathspecContext {
                root: b"/work",
                prefix: b"",
                options,
                unsupported: MagicSet::default(),
                prefer_cwd: false,
                attributes: None,
            };
            Pathspec::parse(&[arg], &cx)
        };
        let lit = PathspecOptions {
            literal: true,
            ..PathspecOptions::default()
        };
        assert!(
            with(lit, "*.rs").unwrap().matches(b"*.rs")
                && !with(lit, "*.rs").unwrap().matches(b"a.rs")
        );
        assert!(with(lit, ":!x").unwrap().matches(b":!x"));
        let glob = PathspecOptions {
            glob: true,
            ..PathspecOptions::default()
        };
        assert!(!with(glob, "*.rs").unwrap().matches(b"d/a.rs"));
        assert!(with(glob, ":(literal)a*").unwrap().matches(b"a*"));
        let noglob = PathspecOptions {
            noglob: true,
            ..PathspecOptions::default()
        };
        assert!(!with(noglob, "*.rs").unwrap().matches(b"a.rs"));
        assert!(with(noglob, ":(glob)*.rs").unwrap().matches(b"a.rs"));
        let both = PathspecOptions {
            glob: true,
            noglob: true,
            ..PathspecOptions::default()
        };
        assert!(with(both, "x").is_err());
        let mixed = PathspecOptions {
            literal: true,
            icase: true,
            ..PathspecOptions::default()
        };
        assert!(with(mixed, "x").is_err());
    }

    #[test]
    fn an_exact_literal_name_stops_a_pathspec_from_matching_later_paths() {
        let s = spec(&[":(icase)a.c"]);
        let (picked, seen) = s.select([b"A.c".as_slice(), b"a.c"]);
        assert_eq!(picked, [b"A.c".as_slice()]);
        assert_eq!(seen, [Some(Match::Exact)]);
        let pattern = spec(&["a*c"]);
        let (picked, _) = pattern.select([b"a*c".as_slice(), b"abc"]);
        assert_eq!(picked.len(), 2);
        assert_eq!(pattern.unmatched([b"zzz".as_slice()]), [0]);
    }

    #[test]
    fn folders_on_the_way_to_a_spec_may_contain_it() {
        let s = spec(&["src/deep/mod.rs", "lib/*.c", ":(glob)**/x"]);
        assert!(s.may_contain(b"src") && s.may_contain(b"src/deep") && s.may_contain(b"lib"));
        assert!(s.may_contain(b"anywhere"), "`**` leads everywhere");
        let t = spec(&["src/deep/mod.rs", "lib/*.c"]);
        assert!(!t.may_contain(b"docs") && !t.may_contain(b"src/other"));
    }
}

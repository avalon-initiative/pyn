//! gitattributes: the per-folder `.gitattributes` files, `.git/info/attributes` and the global attributes file,
//! with git's precedence, macros and pattern rules. The result for a path is the state of every attribute set for it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::wildmatch;

const MAX_LINE: usize = 2048;
const MAX_MACRO_DEPTH: usize = 8;

/// The state of one attribute for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrValue {
    Unspecified,
    Set,
    Unset,
    Value(Vec<u8>),
}

impl AttrValue {
    pub fn is_set(&self) -> bool {
        *self == Self::Set
    }

    pub fn is_unset(&self) -> bool {
        *self == Self::Unset
    }

    pub fn is_unspecified(&self) -> bool {
        *self == Self::Unspecified
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Value(v) => std::str::from_utf8(v).ok(),
            _ => None,
        }
    }
}

type Assignments = Vec<(String, AttrValue)>;

#[derive(Debug, Clone)]
struct Pattern {
    text: Vec<u8>,
    /// No slash in the pattern: it matches the name only, at any depth below its file.
    name_only: bool,
}

#[derive(Debug, Clone)]
struct Line {
    pattern: Pattern,
    attrs: Assignments,
}

#[derive(Debug, Default)]
struct File {
    lines: Vec<Line>,
    macros: Vec<(String, Assignments)>,
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | 0x0b | 0x0c)
}

fn valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name[0] != b'-'
        && name
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub(crate) fn unquote(text: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < text.len() {
        match text[i] {
            b'"' => return Some((out, &text[i + 1..])),
            b'\\' => {
                i += 1;
                match *text.get(i)? {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'r' => out.push(b'\r'),
                    b'v' => out.push(11),
                    b'0'..=b'3' => {
                        let digits = text.get(i..i + 3)?;
                        if !digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                            return None;
                        }
                        let v = digits.iter().fold(0u32, |a, d| a * 8 + u32::from(d - b'0'));
                        out.push(u8::try_from(v).ok()?);
                        i += 2;
                    }
                    c @ (b'\\' | b'"') => out.push(c),
                    _ => return None,
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    None
}

fn parse_assignments(rest: &[u8]) -> Option<Assignments> {
    let mut out = Vec::new();
    for token in rest.split(|b| is_space(*b)).filter(|t| !t.is_empty()) {
        let (name, value) = if let Some(n) = token.strip_prefix(b"-") {
            (n, AttrValue::Unset)
        } else if let Some(n) = token.strip_prefix(b"!") {
            (n, AttrValue::Unspecified)
        } else if let Some(eq) = token.iter().position(|b| *b == b'=') {
            (&token[..eq], AttrValue::Value(token[eq + 1..].to_vec()))
        } else {
            (token, AttrValue::Set)
        };
        if !valid_name(name) {
            return None;
        }
        out.push((String::from_utf8_lossy(name).into_owned(), value));
    }
    Some(out)
}

fn parse(text: &[u8], macros_allowed: bool) -> File {
    let text = text.strip_prefix(b"\xef\xbb\xbf").unwrap_or(text);
    let mut file = File::default();
    for raw in text.split(|b| *b == b'\n') {
        if raw.len() >= MAX_LINE {
            continue;
        }
        let start = raw.iter().position(|b| !is_space(*b)).unwrap_or(raw.len());
        let line = &raw[start..];
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        if let Some(rest) = line.strip_prefix(b"[attr]") {
            if !macros_allowed {
                continue;
            }
            let end = rest.iter().position(|b| is_space(*b)).unwrap_or(rest.len());
            let (name, attrs) = rest.split_at(end);
            if !valid_name(name) {
                continue;
            }
            if let Some(attrs) = parse_assignments(attrs) {
                file.macros
                    .push((String::from_utf8_lossy(name).into_owned(), attrs));
            }
            continue;
        }
        let (pattern, rest) = if line[0] == b'"' {
            match unquote(line) {
                Some(p) => p,
                None => continue,
            }
        } else {
            let end = line.iter().position(|b| is_space(*b)).unwrap_or(line.len());
            (line[..end].to_vec(), &line[end..])
        };
        if pattern.first() == Some(&b'!') || pattern.last() == Some(&b'/') {
            continue;
        }
        let Some(attrs) = parse_assignments(rest) else {
            continue;
        };
        let name_only = !pattern.contains(&b'/');
        let text = pattern.strip_prefix(b"/").unwrap_or(&pattern).to_vec();
        file.lines.push(Line {
            pattern: Pattern { text, name_only },
            attrs,
        });
    }
    file
}

impl Pattern {
    fn matches(&self, rel: &[u8], fold: bool) -> bool {
        if self.name_only {
            let name = rel.rsplit(|b| *b == b'/').next().unwrap_or(rel);
            wildmatch::matches_fold(&self.text, name, false, fold)
        } else {
            wildmatch::matches_fold(&self.text, rel, true, fold)
        }
    }
}

/// Reads the `.gitattributes` of a folder (`""` for the root) as it is in the work tree, or `None`.
pub type Loader = Box<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>;

/// The attribute sources of a repository. Lookups are cached per folder, so build one per operation.
pub struct Attributes {
    global: File,
    info: File,
    loader: Loader,
    fold: bool,
    folders: Mutex<HashMap<Vec<u8>, Arc<File>>>,
}

impl std::fmt::Debug for Attributes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attributes").finish_non_exhaustive()
    }
}

/// Every attribute set for one path, in the order git reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathAttributes {
    values: Assignments,
}

impl PathAttributes {
    /// The state of `name`; unspecified when no rule sets it.
    pub fn get(&self, name: &str) -> AttrValue {
        self.values
            .iter()
            .find(|(n, _)| n == name)
            .map_or(AttrValue::Unspecified, |(_, v)| v.clone())
    }

    /// The attributes that are set, unset or have a value, in first-set order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &AttrValue)> {
        self.values
            .iter()
            .filter(|(_, v)| !v.is_unspecified())
            .map(|(n, v)| (n.as_str(), v))
    }
}

impl Attributes {
    /// `global` and `info` are the contents of the global attributes file and `.git/info/attributes`; `loader`
    /// supplies each folder's `.gitattributes`. With `fold`, patterns match regardless of case (`core.ignorecase`).
    pub fn new(global: &[u8], info: &[u8], loader: Loader, fold: bool) -> Self {
        Self {
            global: parse(global, true),
            info: parse(info, true),
            loader,
            fold,
            folders: Mutex::new(HashMap::new()),
        }
    }

    fn folder(&self, dir: &[u8]) -> Arc<File> {
        let mut cache = self.folders.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = cache.get(dir) {
            return f.clone();
        }
        let file = Arc::new(match (self.loader)(dir) {
            Some(bytes) => parse(&bytes, dir.is_empty()),
            None => File::default(),
        });
        cache.insert(dir.to_vec(), file.clone());
        file
    }

    /// The attributes of `path` (from the root, no trailing slash): the global file, then `.gitattributes` from the
    /// root down to the path's folder, then `.git/info/attributes`; later lines and deeper folders win.
    pub fn for_path(&self, path: &[u8]) -> PathAttributes {
        let mut folders: Vec<(Vec<u8>, Arc<File>)> = Vec::new();
        let mut dir: &[u8] = b"";
        let mut end = 0;
        loop {
            folders.push((dir.to_vec(), self.folder(dir)));
            let Some(slash) = path[end..].iter().position(|b| *b == b'/') else {
                break;
            };
            end += slash;
            dir = &path[..end];
            end += 1;
        }
        let mut macros: HashMap<&str, &Assignments> = HashMap::new();
        macros.insert("binary", &BINARY);
        let sources = std::iter::once((&b""[..], &self.global))
            .chain(folders.iter().map(|(d, f)| (d.as_slice(), &**f)))
            .chain(std::iter::once((&b""[..], &self.info)));
        let sources: Vec<(&[u8], &File)> = sources.collect();
        for (_, file) in &sources {
            for (name, attrs) in &file.macros {
                macros.insert(name, attrs);
            }
        }
        let mut out = PathAttributes::default();
        for (base, file) in sources {
            let rel = if base.is_empty() {
                path
            } else {
                &path[base.len() + 1..]
            };
            for line in &file.lines {
                if line.pattern.matches(rel, self.fold) {
                    for (name, value) in &line.attrs {
                        assign(&mut out.values, &macros, name, value.clone(), 0);
                    }
                }
            }
        }
        out
    }
}

static BINARY: std::sync::LazyLock<Assignments> = std::sync::LazyLock::new(|| {
    vec![
        ("diff".into(), AttrValue::Unset),
        ("merge".into(), AttrValue::Unset),
        ("text".into(), AttrValue::Unset),
    ]
});

fn assign(
    out: &mut Assignments,
    macros: &HashMap<&str, &Assignments>,
    name: &str,
    value: AttrValue,
    depth: usize,
) {
    let expand = value.is_set() && depth < MAX_MACRO_DEPTH;
    match out.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = value,
        None => out.push((name.to_owned(), value)),
    }
    if expand && let Some(body) = macros.get(name) {
        for (n, v) in body.iter() {
            assign(out, macros, n, v.clone(), depth + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(root: &str, subs: &[(&str, &str)], info: &str) -> Attributes {
        let files: HashMap<Vec<u8>, Vec<u8>> =
            std::iter::once((Vec::new(), root.as_bytes().to_vec()))
                .chain(
                    subs.iter()
                        .map(|(d, t)| (d.as_bytes().to_vec(), t.as_bytes().to_vec())),
                )
                .collect();
        Attributes::new(
            b"",
            info.as_bytes(),
            Box::new(move |d| files.get(d).cloned()),
            false,
        )
    }

    fn get(a: &Attributes, path: &str, name: &str) -> AttrValue {
        a.for_path(path.as_bytes()).get(name)
    }

    #[test]
    fn states_and_last_line_wins() {
        let a = engine(
            "*.txt text\n*.txt -text\nx.txt text=auto eol=crlf\ny.txt !text\n",
            &[],
            "",
        );
        assert_eq!(get(&a, "a.txt", "text"), AttrValue::Unset);
        assert_eq!(get(&a, "x.txt", "text"), AttrValue::Value(b"auto".to_vec()));
        assert_eq!(get(&a, "x.txt", "eol").as_str(), Some("crlf"));
        assert!(get(&a, "y.txt", "text").is_unspecified());
        assert!(get(&a, "a.c", "text").is_unspecified());
    }

    #[test]
    fn deeper_folders_and_info_take_precedence() {
        let a = engine(
            "*.c text\n",
            &[("sub", "*.c -text\n")],
            "sub/x.c text=auto\n",
        );
        assert!(get(&a, "top.c", "text").is_set());
        assert!(get(&a, "sub/y.c", "text").is_unset());
        assert_eq!(get(&a, "sub/x.c", "text").as_str(), Some("auto"));
    }

    #[test]
    fn patterns_without_slash_match_names_and_others_are_anchored() {
        let a = engine("*.c a\nsub/*.h b\n/top c\n**/deep/x d\n", &[], "");
        assert!(get(&a, "p/q/r.c", "a").is_set());
        assert!(get(&a, "sub/x.h", "b").is_set());
        assert!(get(&a, "o/sub/x.h", "b").is_unspecified());
        assert!(get(&a, "top", "c").is_set());
        assert!(get(&a, "o/top", "c").is_unspecified());
        assert!(get(&a, "o/deep/x", "d").is_set());
    }

    #[test]
    fn negative_and_directory_patterns_are_ignored_and_quoting_works() {
        let a = engine("!x a\ndir/ b\n\"q s.txt\" c\n", &[], "");
        assert!(get(&a, "x", "a").is_unspecified());
        assert!(get(&a, "dir/f", "b").is_unspecified());
        assert!(get(&a, "q s.txt", "c").is_set());
    }

    #[test]
    fn macros_expand_when_set_and_only_from_top_level_files() {
        let a = engine(
            "[attr]mine -text eol=crlf\n*.m mine\n*.b binary\n*.n -binary\n",
            &[("sub", "[attr]late foo\n*.z late\n")],
            "",
        );
        assert!(get(&a, "f.m", "mine").is_set());
        assert!(get(&a, "f.m", "text").is_unset());
        assert_eq!(get(&a, "f.m", "eol").as_str(), Some("crlf"));
        assert!(get(&a, "f.b", "diff").is_unset());
        assert!(get(&a, "f.b", "merge").is_unset());
        assert!(get(&a, "f.n", "text").is_unspecified());
        assert!(get(&a, "sub/f.z", "late").is_set());
        assert!(get(&a, "sub/f.z", "foo").is_unspecified());
    }

    #[test]
    fn a_macro_line_overrides_earlier_attributes_and_later_ones_override_it() {
        let a = engine("*.b diff\n*.b binary\n*.b text\n", &[], "");
        assert!(get(&a, "f.b", "diff").is_unset());
        assert!(get(&a, "f.b", "text").is_set());
    }
}

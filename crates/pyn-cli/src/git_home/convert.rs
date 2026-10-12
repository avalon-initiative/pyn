//! Refuses to write a file that git would convert on the way into the work tree (line endings, filters,
//! `$Id$`, encodings): pyn writes blob bytes as they are, so it stops rather than write different bytes (#273).

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};
use pyn_gitwork::{Ignore, Level, parse_rules};

use super::{GitHome, path_string, user_dirs};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Attr {
    Unspecified,
    Set,
    Unset,
    Value(String),
}

#[derive(Debug, Clone)]
struct Conv {
    text: Attr,
    eol: Attr,
    crlf: Attr,
    filter: Attr,
    ident: Attr,
    encoding: Attr,
}

impl Conv {
    fn new() -> Self {
        let none = || Attr::Unspecified;
        Self {
            text: none(),
            eol: none(),
            crlf: none(),
            filter: none(),
            ident: none(),
            encoding: none(),
        }
    }

    fn apply(&mut self, name: &str, value: Attr) {
        match name {
            "text" => self.text = value,
            "eol" => self.eol = value,
            "crlf" => self.crlf = value,
            "filter" => self.filter = value,
            "ident" => self.ident = value,
            "working-tree-encoding" => self.encoding = value,
            "binary" if value == Attr::Set => self.text = Attr::Unset,
            _ => {}
        }
    }
}

fn spec(token: &str) -> (&str, Attr) {
    if let Some(name) = token.strip_prefix('-') {
        (name, Attr::Unset)
    } else if let Some(name) = token.strip_prefix('!') {
        (name, Attr::Unspecified)
    } else if let Some((name, value)) = token.split_once('=') {
        (name, Attr::Value(value.to_string()))
    } else {
        (token, Attr::Set)
    }
}

/// The conversion attributes that apply to `path` from one attributes file whose folder is `base`.
fn apply_file(conv: &mut Conv, text: &[u8], base: &[u8], path: &[u8]) {
    let ignore = Ignore::default();
    for line in String::from_utf8_lossy(text).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let Some(pattern) = tokens.next() else {
            continue;
        };
        let level = Level::new(base, parse_rules(pattern.as_bytes()));
        if !path.starts_with(base) || !ignore.is_ignored(&[level], path, false) {
            continue;
        }
        for token in tokens {
            let (name, value) = spec(token);
            conv.apply(name, value);
        }
    }
}

fn read(path: &std::path::Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_default()
}

fn global_attributes(git: &GitHome) -> Option<PathBuf> {
    let (home, xdg) = user_dirs();
    match git.repo.config().get("core.attributesfile") {
        Some(p) if !p.is_empty() => Some(match p.strip_prefix("~/") {
            Some(rest) => home?.join(rest),
            None => PathBuf::from(p),
        }),
        _ => Some(
            xdg.or_else(|| home.map(|h| h.join(".config")))?
                .join("git/attributes"),
        ),
    }
}

fn attributes_of(git: &GitHome, path: &[u8]) -> Result<Conv> {
    let mut conv = Conv::new();
    let root = git.repo.root()?;
    if let Some(file) = global_attributes(git) {
        apply_file(&mut conv, &read(&file), b"", path);
    }
    let mut dir_end = 0;
    let mut dir: Vec<u8> = Vec::new();
    loop {
        let base = if dir.is_empty() {
            Vec::new()
        } else {
            [dir.as_slice(), b"/"].concat()
        };
        let file = pyn_gitwork::fs_path(root, &dir).join(".gitattributes");
        apply_file(&mut conv, &read(&file), &base, path);
        let Some(slash) = path[dir_end..].iter().position(|b| *b == b'/') else {
            break;
        };
        dir_end += slash;
        dir = path[..dir_end].to_vec();
        dir_end += 1;
    }
    let info = git.repo.git().common_dir().join("info/attributes");
    apply_file(&mut conv, &read(&info), b"", path);
    Ok(conv)
}

/// Why git would convert `path` when writing it, if it would.
fn why(git: &GitHome, conv: &Conv) -> Option<String> {
    match &conv.filter {
        Attr::Value(f) if f == "lfs" => {}
        Attr::Value(f) => return Some(format!("the '{f}' filter applies")),
        Attr::Set => return Some("a filter applies".into()),
        _ => {}
    }
    if matches!(conv.ident, Attr::Set | Attr::Value(_)) {
        return Some("the ident attribute applies".into());
    }
    if matches!(conv.encoding, Attr::Value(_)) {
        return Some("working-tree-encoding applies".into());
    }
    let text_off =
        conv.text == Attr::Unset || (conv.text == Attr::Unspecified && conv.crlf == Attr::Unset);
    if text_off {
        return None;
    }
    if conv.eol == Attr::Value("crlf".into()) {
        return Some("eol=crlf applies".into());
    }
    if conv.eol == Attr::Value("lf".into()) {
        return None;
    }
    if git.repo.bool_setting("core.autocrlf", false) {
        return Some("core.autocrlf is set".into());
    }
    let text_on =
        !matches!(conv.text, Attr::Unspecified) || matches!(conv.crlf, Attr::Set | Attr::Value(_));
    if text_on
        && git
            .repo
            .config()
            .get("core.eol")
            .is_some_and(|v| v.eq_ignore_ascii_case("crlf"))
    {
        return Some("core.eol=crlf applies".into());
    }
    None
}

/// Fails, naming the first path git would convert; warns once about paths a Git LFS filter owns.
pub(super) fn refuse_conversions<'a>(
    git: &GitHome,
    paths: impl IntoIterator<Item = &'a [u8]>,
) -> Result<()> {
    let mut cache: BTreeMap<Vec<u8>, ()> = BTreeMap::new();
    let mut lfs = false;
    for path in paths {
        if cache.insert(path.to_vec(), ()).is_some() {
            continue;
        }
        let conv = attributes_of(git, path)?;
        if conv.filter == Attr::Value("lfs".into()) {
            lfs = true;
        }
        if let Some(reason) = why(git, &conv) {
            bail!(
                "git would convert '{}' when writing it ({reason}), and pyn does not convert line endings or \
                 apply filters yet (#273); nothing was changed",
                path_string(path)
            );
        }
    }
    if lfs {
        eprintln!(
            "warning: some paths use the Git LFS filter; pyn has no LFS client, so pointer files are written as they are"
        );
    }
    Ok(())
}

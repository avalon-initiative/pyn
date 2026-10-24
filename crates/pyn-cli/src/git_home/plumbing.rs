//! Pieces shared by the inspection commands: object lookup by any revision form, abbreviations and exits.

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use pyn_git::Ref;
use pyn_object::{Kind, ObjectId, Tag};

use super::GitHome;

/// Prints `message` as git does for a bad option and exits with its usage status.
pub(super) fn usage(message: &str) -> ! {
    eprintln!("error: {message}");
    std::process::exit(129);
}

/// Writes raw bytes to standard output; a closed pipe ends the command quietly.
pub(super) fn emit_bytes(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(bytes).and_then(|()| out.flush())
        && e.kind() == std::io::ErrorKind::BrokenPipe
    {
        std::process::exit(0);
    }
}

/// The folder `cwd` is in, from the work tree root, with a trailing `/` (empty at the root).
pub(super) fn prefix(git: &GitHome, cwd: &Path) -> Result<Vec<u8>> {
    let p = git.ws.repo_prefix(cwd, ".")?;
    Ok(if p.is_empty() {
        Vec::new()
    } else {
        format!("{p}/").into_bytes()
    })
}

pub(super) fn quote(git: &GitHome, path: &[u8]) -> String {
    crate::short_status::quote_diff(path, git.repo.bool_setting("core.quotePath", true))
}

/// The abbreviation length `--short` and friends start from: `core.abbrev`, else 7.
pub(super) fn default_abbrev(git: &GitHome) -> usize {
    let hex = git.algorithm().digest_len() * 2;
    match git.config().get("core.abbrev") {
        Some(v) if v.eq_ignore_ascii_case("no") => hex,
        Some(v) => v.parse::<usize>().map_or(7, |n| n.clamp(4, hex)),
        None => 7,
    }
}

/// The shortest prefix of at least `min` digits that no other object shares.
pub(super) fn abbrev(git: &GitHome, id: &ObjectId, min: usize) -> String {
    let hex = id.to_hex();
    let mut n = min.clamp(4, hex.len());
    while n < hex.len() {
        match git.repo.ids_with_prefix(&hex[..n]) {
            Ok(found) if found.len() > 1 => n += 1,
            _ => break,
        }
    }
    hex[..n].to_string()
}

pub(super) fn kind_of(git: &GitHome, id: &ObjectId) -> Result<Option<Kind>> {
    Ok(git.repo.objects().read_opt(id)?.map(|o| o.kind))
}

/// Follows tags from `id` to the first object of kind `want`, or to a non-tag object when `want` is `None`.
pub(super) fn peel(git: &GitHome, id: &ObjectId, want: Option<Kind>) -> Result<Option<ObjectId>> {
    let mut current = *id;
    loop {
        let Some(object) = git.repo.objects().read_opt(&current)? else {
            return Ok(None);
        };
        match (object.kind, want) {
            (k, Some(w)) if k == w => return Ok(Some(current)),
            (Kind::Tag, _) => {
                current = Tag::parse(git.algorithm(), &object.data)?.object;
            }
            (Kind::Commit, Some(Kind::Tree)) => {
                return Ok(Some(git.repo.commit(&current)?.tree));
            }
            (_, None) => return Ok(Some(current)),
            _ => return Ok(None),
        }
    }
}

/// The full name of the ref `name` stands for, with symbolic refs followed (`HEAD` gives the branch).
pub(super) fn dwim(git: &GitHome, name: &str) -> Result<Option<String>> {
    let Some(mut full) = git.repo.expand_ref_name(name)? else {
        return Ok(None);
    };
    for _ in 0..5 {
        match git.repo.refs().read(&full)? {
            Some(Ref::Symbolic(target)) => full = target,
            _ => break,
        }
    }
    Ok(Some(full))
}

const RULES: [(&str, &str); 6] = [
    ("", ""),
    ("refs/", ""),
    ("refs/tags/", ""),
    ("refs/heads/", ""),
    ("refs/remotes/", ""),
    ("refs/remotes/", "/HEAD"),
];

/// The shortest name for `full` that still names only it, as `--abbrev-ref` prints.
pub(super) fn shorten_ref(git: &GitHome, full: &str, strict: bool) -> String {
    let exists = |name: &str| git.repo.refs().read(name).ok().flatten().is_some();
    for i in (1..RULES.len()).rev() {
        let (head, tail) = RULES[i];
        let Some(short) = full
            .strip_prefix(head)
            .and_then(|s| s.strip_suffix(tail))
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let limit = if strict { RULES.len() } else { i };
        let clash = (0..limit)
            .filter(|j| *j != i)
            .any(|j| exists(&format!("{}{short}{}", RULES[j].0, RULES[j].1)));
        if !clash {
            return short.to_string();
        }
    }
    full.to_string()
}

/// Revision names of every form `rev-parse` takes, resolved to the object they name without peeling.
pub(super) struct Resolver<'a> {
    pub git: &'a GitHome,
    pub prefix: &'a [u8],
}

impl Resolver<'_> {
    /// The object `spec` names, or `None` when it names nothing.
    pub fn object(&self, spec: &str) -> Result<Option<ObjectId>> {
        if let Some(rest) = spec.strip_prefix(':') {
            return self.index_entry(rest);
        }
        if let Some(at) = tree_colon(spec) {
            let Some(tree) = self.rev(&spec[..at])? else {
                return Ok(None);
            };
            let Some(tree) = peel(self.git, &tree, Some(Kind::Tree))? else {
                return Ok(None);
            };
            let path = self.join(&spec[at + 1..]);
            if path.is_empty() {
                return Ok(Some(tree));
            }
            return Ok(self.git.repo.entry_at(&tree, &path)?.map(|e| e.id));
        }
        self.rev(spec)
    }

    fn join(&self, path: &str) -> Vec<u8> {
        let Some(rest) = path
            .strip_prefix("./")
            .or_else(|| path.strip_prefix("../"))
            .map(|_| path)
        else {
            return path.trim_end_matches('/').as_bytes().to_vec();
        };
        let mut parts: Vec<&[u8]> = self
            .prefix
            .split(|b| *b == b'/')
            .filter(|p| !p.is_empty())
            .collect();
        for part in rest.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                other => parts.push(other.as_bytes()),
            }
        }
        parts.join(&b'/')
    }

    fn index_entry(&self, rest: &str) -> Result<Option<ObjectId>> {
        let (stage, path) = match rest.as_bytes() {
            [d @ b'0'..=b'3', b':', ..] => (d - b'0', &rest[2..]),
            _ => (0, rest),
        };
        if path.starts_with('/') || path.is_empty() {
            return Ok(None);
        }
        let path = self.join(path);
        let index = self.git.repo.read_index()?;
        Ok(index
            .entries
            .iter()
            .find(|e| e.stage == stage && e.path == path)
            .map(|e| e.id))
    }

    /// A revision expression: a base name followed by `~`, `^` and `^{...}` steps.
    pub fn rev(&self, spec: &str) -> Result<Option<ObjectId>> {
        let (base, mut ops) = split_base(spec);
        if base.is_empty() {
            return Ok(None);
        }
        let Some(mut id) = self.base(base)? else {
            return Ok(None);
        };
        while let Some(op) = ops.chars().next() {
            ops = &ops[1..];
            if op == '^'
                && let Some(inner) = ops.strip_prefix('{')
            {
                let Some(end) = inner.find('}') else {
                    return Ok(None);
                };
                let want = match &inner[..end] {
                    "" => None,
                    "object" => {
                        if kind_of(self.git, &id)?.is_none() {
                            return Ok(None);
                        }
                        ops = &inner[end + 1..];
                        continue;
                    }
                    name => match Kind::from_name(name.as_bytes()) {
                        Some(k) => Some(k),
                        None => return Ok(None),
                    },
                };
                ops = &inner[end + 1..];
                match peel(self.git, &id, want)? {
                    Some(p) => id = p,
                    None => return Ok(None),
                }
                continue;
            }
            let digits = ops.find(|c: char| !c.is_ascii_digit()).unwrap_or(ops.len());
            let n: Option<usize> = ops[..digits].parse().ok();
            ops = &ops[digits..];
            let Some(commit) = peel(self.git, &id, Some(Kind::Commit))? else {
                return Ok(None);
            };
            id = commit;
            let (nth, times) = match (op, n) {
                ('~', n) => (1, n.unwrap_or(1)),
                ('^', Some(0)) => continue,
                ('^', n) => (n.unwrap_or(1), 1),
                _ => return Ok(None),
            };
            for _ in 0..times {
                match self.git.repo.commit(&id)?.parents.get(nth - 1) {
                    Some(p) => id = *p,
                    None => return Ok(None),
                }
            }
        }
        Ok(Some(id))
    }

    fn base(&self, base: &str) -> Result<Option<ObjectId>> {
        let git = self.git;
        if base.contains("@{") {
            return Ok(git.repo.resolve(base).ok());
        }
        let hex_len = git.algorithm().digest_len() * 2;
        if base.len() == hex_len
            && let Ok(id) = ObjectId::from_hex(git.algorithm(), &base.to_ascii_lowercase())
        {
            return Ok(Some(id));
        }
        if let Some(full) = git.repo.expand_ref_name(base)? {
            return Ok(git.repo.refs().resolve(&full)?);
        }
        if base.len() >= 4 && base.bytes().all(|b| b.is_ascii_hexdigit()) {
            let found = git.repo.ids_with_prefix(&base.to_ascii_lowercase())?;
            if let [only] = found.as_slice() {
                return Ok(Some(*only));
            }
        }
        Ok(None)
    }
}

/// Splits a revision at its first `~` or `^` outside an `@{...}`.
fn split_base(spec: &str) -> (&str, &str) {
    let bytes = spec.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'@' if bytes.get(i + 1) == Some(&b'{') => {
                i += spec[i..].find('}').map_or(bytes.len() - i, |e| e + 1);
                continue;
            }
            b'~' | b'^' => break,
            _ => {}
        }
        i += 1;
    }
    spec.split_at(i)
}

/// The position of the `:` that separates `<tree-ish>` from a path, outside an `@{...}`.
fn tree_colon(spec: &str) -> Option<usize> {
    let bytes = spec.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'@' if bytes.get(i + 1) == Some(&b'{') => {
                i += spec[i..].find('}').map_or(bytes.len() - i, |e| e + 1);
                continue;
            }
            b':' => return (i > 0).then_some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

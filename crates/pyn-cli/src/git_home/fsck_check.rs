//! The object checks of `fsck`: the same findings, ids and wording as git's, for commits, trees and tags.

use pyn_object::{Algorithm, ObjectId, is_git_dir_name};

use super::storage_links::tree_entries;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Severity {
    /// Printed as a warning; `--strict` makes it an error.
    Warning,
    /// Printed as a warning whatever the mode.
    Info,
    Error,
}

pub(super) struct Finding {
    pub severity: Severity,
    pub id: &'static str,
    pub text: &'static str,
}

const fn warn(id: &'static str, text: &'static str) -> Finding {
    Finding {
        severity: Severity::Warning,
        id,
        text,
    }
}

const fn info(id: &'static str, text: &'static str) -> Finding {
    Finding {
        severity: Severity::Info,
        id,
        text,
    }
}

const fn error(id: &'static str, text: &'static str) -> Finding {
    Finding {
        severity: Severity::Error,
        id,
        text,
    }
}

/// An object that cannot be parsed at all: the lines git prints before saying so.
pub(super) struct Unparsable(pub Vec<String>);

pub(super) type Checked = Result<Vec<Finding>, Unparsable>;

fn terminated(body: &[u8]) -> Option<Finding> {
    for (i, b) in body.iter().enumerate() {
        if *b == 0 {
            return Some(error("nulInHeader", "unterminated header: NUL"));
        }
        if *b == b'\n' && body.get(i + 1) == Some(&b'\n') {
            return None;
        }
    }
    (body.last() != Some(&b'\n')).then(|| error("unterminatedHeader", "unterminated header"))
}

fn line_after<'a>(body: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    body.strip_prefix(prefix)
        .map(|r| r.split(|b| *b == b'\n').next().unwrap_or(r))
}

/// Checks an `author`/`committer`/`tagger` value (everything after the keyword).
fn ident(value: &[u8]) -> Option<Finding> {
    if value.first() == Some(&b'<') {
        return Some(error(
            "missingNameBeforeEmail",
            "invalid author/committer line - missing space before email",
        ));
    }
    let stop = |from: usize| {
        value[from..]
            .iter()
            .position(|b| matches!(b, b'<' | b'>'))
            .map_or(value.len(), |p| from + p)
    };
    let lt = stop(0);
    match value.get(lt) {
        Some(b'>') => {
            return Some(error("badName", "invalid author/committer line - bad name"));
        }
        Some(b'<') => {}
        _ => {
            return Some(error(
                "missingEmail",
                "invalid author/committer line - missing email",
            ));
        }
    }
    if lt == 0 || value[lt - 1] != b' ' {
        return Some(error(
            "missingSpaceBeforeEmail",
            "invalid author/committer line - missing space before email",
        ));
    }
    let gt = stop(lt + 1);
    if value.get(gt) != Some(&b'>') {
        return Some(error(
            "badEmail",
            "invalid author/committer line - bad email",
        ));
    }
    let rest = &value[gt + 1..];
    let Some(rest) = rest.strip_prefix(b" ") else {
        return Some(error(
            "missingSpaceBeforeDate",
            "invalid author/committer line - missing space before date",
        ));
    };
    if rest.first() == Some(&b'0') && rest.get(1) != Some(&b' ') {
        return Some(warn(
            "zeroPaddedDate",
            "invalid author/committer line - date is zero-padded",
        ));
    }
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || rest.get(digits) != Some(&b' ') {
        return Some(error("badDate", "invalid author/committer line - bad date"));
    }
    if std::str::from_utf8(&rest[..digits])
        .ok()
        .and_then(|d| d.parse::<u64>().ok())
        .is_none()
    {
        return Some(error(
            "badDateOverflow",
            "invalid author/committer line - date causes integer overflow",
        ));
    }
    let tz = &rest[digits + 1..];
    let ok =
        tz.len() == 5 && matches!(tz[0], b'+' | b'-') && tz[1..5].iter().all(u8::is_ascii_digit);
    (!ok).then(|| {
        error(
            "badTimezone",
            "invalid author/committer line - bad time zone",
        )
    })
}

pub(super) fn commit(alg: Algorithm, id: &ObjectId, body: &[u8]) -> Checked {
    let hex = alg.digest_len() * 2;
    let valid_hex = |v: &[u8]| v.len() == hex && v.iter().all(u8::is_ascii_hexdigit);
    let mut rest = body;
    match line_after(rest, b"tree ") {
        Some(v) if valid_hex(v) => rest = &rest[5 + hex..],
        _ => return Err(Unparsable(vec![format!("error: bogus commit object {id}")])),
    }
    rest = rest.strip_prefix(b"\n").unwrap_or(rest);
    while let Some(v) = line_after(rest, b"parent ") {
        if !valid_hex(v) {
            return Err(Unparsable(vec![format!(
                "error: bad parents in commit {id}"
            )]));
        }
        rest = &rest[7 + hex..];
        rest = rest.strip_prefix(b"\n").unwrap_or(rest);
    }
    let mut out = Vec::new();
    if let Some(f) = terminated(body) {
        out.push(f);
        return Ok(out);
    }
    let Some(author) = line_after(rest, b"author ") else {
        out.push(error(
            "missingAuthor",
            "invalid format - expected 'author' line",
        ));
        return Ok(out);
    };
    out.extend(ident(author));
    rest = &rest[7 + author.len()..];
    rest = rest.strip_prefix(b"\n").unwrap_or(rest);
    let Some(committer) = line_after(rest, b"committer ") else {
        out.push(error(
            "missingCommitter",
            "invalid format - expected 'committer' line",
        ));
        return Ok(out);
    };
    out.extend(ident(committer));
    Ok(out)
}

pub(super) fn tag(alg: Algorithm, body: &[u8]) -> Checked {
    let hex = alg.digest_len() * 2;
    let unparsable = || Unparsable(Vec::new());
    let obj = line_after(body, b"object ").ok_or_else(unparsable)?;
    if obj.len() != hex || !obj.iter().all(u8::is_ascii_hexdigit) {
        return Err(unparsable());
    }
    let mut rest = &body[8 + hex..];
    let ty = line_after(rest, b"type ").ok_or_else(unparsable)?;
    rest = &rest[6 + ty.len()..];
    let name = line_after(rest, b"tag ").ok_or_else(unparsable)?;
    rest = &rest[5 + name.len()..];
    let mut out = Vec::new();
    if let Some(f) = terminated(body) {
        out.push(f);
        return Ok(out);
    }
    match line_after(rest, b"tagger ") {
        Some(t) => out.extend(ident(t)),
        None => out.push(info(
            "missingTaggerEntry",
            "invalid format - expected 'tagger' line",
        )),
    }
    Ok(out)
}

pub(super) fn tree(alg: Algorithm, body: &[u8], strict: bool) -> Checked {
    let entries =
        tree_entries(alg, body).map_err(|why| Unparsable(vec![format!("error: {why}")]))?;
    let mut zero_padded = false;
    let mut full = false;
    let mut dot = false;
    let mut dotdot = false;
    let mut dotgit = false;
    let mut bad_mode = false;
    let mut null = false;
    let mut dup = false;
    let mut unsorted = false;
    let zero = vec![0u8; alg.digest_len()];
    let is_dir = |mode: &[u8]| {
        u32::from_str_radix(std::str::from_utf8(mode).unwrap_or("0"), 8)
            .is_ok_and(|m| m & 0o170000 == 0o040000)
    };
    let mut prev: Option<(&[u8], bool)> = None;
    for e in &entries {
        zero_padded |= e.mode.first() == Some(&b'0');
        full |= e.name.contains(&b'/');
        dot |= e.name == b".";
        dotdot |= e.name == b"..";
        dotgit |= is_git_dir_name(e.name);
        null |= e.id.as_bytes() == zero.as_slice();
        let mode = u32::from_str_radix(std::str::from_utf8(e.mode).unwrap_or("0"), 8).unwrap_or(0);
        bad_mode |= match mode {
            0o100644 | 0o100755 | 0o120000 | 0o040000 | 0o160000 => false,
            0o100664 => strict,
            _ => true,
        };
        let dir = is_dir(e.mode);
        if let Some((pname, pdir)) = prev {
            if pname == e.name {
                dup = true;
            } else {
                let key = |n: &[u8], d: bool| {
                    let mut k = n.to_vec();
                    if d {
                        k.push(b'/');
                    }
                    k
                };
                if key(pname, pdir) > key(e.name, dir) {
                    unsorted = true;
                }
            }
        }
        prev = Some((e.name, dir));
    }
    let mut out = Vec::new();
    let mut add = |on: bool, f: Finding| {
        if on {
            out.push(f);
        }
    };
    add(
        zero_padded,
        warn("zeroPaddedFilemode", "contains zero-padded file modes"),
    );
    add(
        dup,
        error("duplicateEntries", "contains duplicate file entries"),
    );
    add(dot, warn("hasDot", "contains '.'"));
    add(dotdot, warn("hasDotdot", "contains '..'"));
    add(dotgit, warn("hasDotgit", "contains '.git'"));
    add(unsorted, error("treeNotSorted", "not properly sorted"));
    add(bad_mode, info("badFilemode", "contains bad file modes"));
    add(full, warn("fullPathname", "contains full pathnames"));
    add(
        null,
        warn("nullSha1", "contains entries pointing to null sha1"),
    );
    Ok(out)
}

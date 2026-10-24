//! Reading patches as `git apply` does: git headers, traditional `---`/`+++` headers and hunks, with the same
//! line counting and the same errors.

use std::sync::atomic::AtomicBool;

use super::apply_ws::check_added;

/// While set, diagnostics are not printed: `am --3way` tries a plain apply first without reporting it.
pub(super) static QUIET: AtomicBool = AtomicBool::new(false);

macro_rules! diag {
    ($($t:tt)*) => {
        if !$crate::git_home::apply_parse::QUIET.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!($($t)*);
        }
    };
}
pub(crate) use diag;

/// A failure whose message was already printed; the value is the exit status.
#[derive(Debug, Clone, Copy)]
pub(super) struct Fail(pub i32);

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "apply failed ({})", self.0)
    }
}

impl std::error::Error for Fail {}

/// A hunk with its lines as `(marker, bytes)`; `bytes` lacks the newline when the patch marks the end of file.
#[derive(Debug, Clone)]
pub(super) struct Hunk {
    pub oldpos: usize,
    pub oldlines: usize,
    pub newpos: usize,
    pub newlines: usize,
    pub leading: usize,
    pub trailing: usize,
    pub lines: Vec<(u8, Vec<u8>)>,
    /// The patch line number of the hunk header.
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Binary {
    No,
    /// `Binary files a and b differ`, which carries no data.
    Marker,
    /// `GIT binary patch`.
    Data,
}

#[derive(Debug, Clone)]
pub(super) struct FilePatch {
    pub old_name: Option<Vec<u8>>,
    pub new_name: Option<Vec<u8>>,
    pub old_mode: u32,
    pub new_mode: u32,
    /// 1 for a creation, -1 when the patch only looks like one (an empty old side).
    pub is_new: i8,
    pub is_delete: bool,
    pub is_rename: bool,
    pub is_copy: bool,
    pub score: u32,
    pub old_id: Option<String>,
    pub new_id: Option<String>,
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub deleted: usize,
    pub binary: Binary,
    pub git_header: bool,
}

impl FilePatch {
    fn new() -> Self {
        Self {
            old_name: None,
            new_name: None,
            old_mode: 0,
            new_mode: 0,
            is_new: 0,
            is_delete: false,
            is_rename: false,
            is_copy: false,
            score: 0,
            old_id: None,
            new_id: None,
            hunks: Vec::new(),
            added: 0,
            deleted: 0,
            binary: Binary::No,
            git_header: false,
        }
    }

    /// The name the patch is known by: the new one, else the old one.
    pub fn name(&self) -> &[u8] {
        self.new_name
            .as_deref()
            .or(self.old_name.as_deref())
            .unwrap_or_default()
    }

    /// Turns the patch around, as `-R` does.
    pub fn reverse(&mut self) {
        std::mem::swap(&mut self.old_name, &mut self.new_name);
        std::mem::swap(&mut self.old_mode, &mut self.new_mode);
        std::mem::swap(&mut self.old_id, &mut self.new_id);
        std::mem::swap(&mut self.added, &mut self.deleted);
        let was_new = self.is_new > 0;
        self.is_new = i8::from(self.is_delete);
        self.is_delete = was_new;
        for h in &mut self.hunks {
            std::mem::swap(&mut h.oldpos, &mut h.newpos);
            std::mem::swap(&mut h.oldlines, &mut h.newlines);
            std::mem::swap(&mut h.leading, &mut h.trailing);
            for (marker, _) in &mut h.lines {
                *marker = match *marker {
                    b'+' => b'-',
                    b'-' => b'+',
                    m => m,
                };
            }
        }
    }
}

pub(super) struct Parsed {
    pub patches: Vec<FilePatch>,
    pub ws_errors: usize,
}

pub(super) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    /// 1-based number of the line at `pos`.
    line: usize,
    strip: usize,
    label: &'a str,
    ws: bool,
    ws_errors: usize,
}

fn line_at(buf: &[u8], pos: usize) -> &[u8] {
    let rest = &buf[pos..];
    match rest.iter().position(|b| *b == b'\n') {
        Some(i) => &rest[..=i],
        None => rest,
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\n')
        .to_string()
}

/// `a/b/c` with `n` leading components removed; `None` when fewer remain.
fn strip_components(name: &[u8], n: usize) -> Option<Vec<u8>> {
    let mut rest = name;
    for _ in 0..n {
        let slash = rest.iter().position(|b| *b == b'/')?;
        let mut next = &rest[slash + 1..];
        while let [b'/', tail @ ..] = next {
            next = tail;
        }
        rest = next;
    }
    (!rest.is_empty()).then(|| rest.to_vec())
}

/// Reads a C-quoted name at the start of `s`; returns the bytes and what follows the closing quote.
fn unquote(s: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < s.len() {
        match s[i] {
            b'"' => return Some((out, &s[i + 1..])),
            b'\\' => {
                i += 1;
                let c = *s.get(i)?;
                match c {
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(11),
                    b'"' | b'\\' => out.push(c),
                    b'0'..=b'3' => {
                        let digits = s.get(i..i + 3)?;
                        if !digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                            return None;
                        }
                        let v = digits
                            .iter()
                            .fold(0u32, |acc, d| acc * 8 + u32::from(d - b'0'));
                        out.push(u8::try_from(v).ok()?);
                        i += 2;
                    }
                    _ => return None,
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    None
}

/// The name on a `---`, `+++`, `rename` or `copy` line: up to a tab or the end of the line, `/dev/null` as `None`.
fn header_name(rest: &[u8], strip: usize) -> Option<Option<Vec<u8>>> {
    let rest = rest.strip_suffix(b"\n").unwrap_or(rest);
    let raw = if rest.first() == Some(&b'"') {
        unquote(rest)?.0
    } else {
        rest.split(|b| *b == b'\t')
            .next()
            .unwrap_or_default()
            .to_vec()
    };
    if raw == b"/dev/null" {
        return Some(None);
    }
    strip_components(&raw, strip).map(Some)
}

/// The name both sides of a `diff --git a/x b/x` line share, if they do.
fn git_header_name(rest: &[u8], strip: usize) -> Option<Vec<u8>> {
    let rest = rest.strip_suffix(b"\n").unwrap_or(rest);
    if rest.first() == Some(&b'"') {
        let (first, after) = unquote(rest)?;
        let first = strip_components(&first, strip)?;
        let after = after.strip_prefix(b" ")?;
        let second = if after.first() == Some(&b'"') {
            unquote(after)?.0
        } else {
            after.to_vec()
        };
        return (strip_components(&second, strip)? == first).then_some(first);
    }
    for (i, b) in rest.iter().enumerate() {
        if *b != b' ' {
            continue;
        }
        let (left, right) = (&rest[..i], &rest[i + 1..]);
        let right = if right.first() == Some(&b'"') {
            match unquote(right) {
                Some((name, [])) => name,
                _ => continue,
            }
        } else {
            right.to_vec()
        };
        if let (Some(l), Some(r)) = (
            strip_components(left, strip),
            strip_components(&right, strip),
        ) && l == r
        {
            return Some(l);
        }
    }
    None
}

fn octal(rest: &[u8]) -> u32 {
    let digits: Vec<u8> = rest
        .iter()
        .copied()
        .take_while(|b| (b'0'..=b'7').contains(b))
        .collect();
    u32::from_str_radix(&String::from_utf8_lossy(&digits), 8).unwrap_or(0)
}

/// `@@ -a[,b] +c[,d] @@` as numbers, or `None`.
fn hunk_header(line: &[u8]) -> Option<(usize, usize, usize, usize)> {
    let s = std::str::from_utf8(line.strip_prefix(b"@@ -")?).ok()?;
    let (old, rest) = s.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let pair = |t: &str| -> Option<(usize, usize)> {
        match t.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((t.parse().ok()?, 1)),
        }
    };
    let (op, ol) = pair(old)?;
    let (np, nl) = pair(new)?;
    Some((op, ol, np, nl))
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8], label: &'a str, strip: usize, ws: bool) -> Self {
        Self {
            buf,
            pos: 0,
            line: 1,
            strip,
            label,
            ws,
            ws_errors: 0,
        }
    }

    fn peek(&self) -> &'a [u8] {
        line_at(self.buf, self.pos)
    }

    fn advance(&mut self) {
        self.pos += self.peek().len();
        self.line += 1;
    }

    fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub fn parse(mut self) -> Result<Parsed, Fail> {
        let mut patches = Vec::new();
        while !self.done() {
            match self.chunk()? {
                Some(p) => patches.push(p),
                None => break,
            }
        }
        Ok(Parsed {
            patches,
            ws_errors: self.ws_errors,
        })
    }

    /// The next patch, or `None` when no header is left.
    fn chunk(&mut self) -> Result<Option<FilePatch>, Fail> {
        let Some(mut patch) = self.find_header()? else {
            return Ok(None);
        };
        let start = self.line;
        self.fragments(&mut patch)?;
        if patch.hunks.is_empty() && patch.binary == Binary::No {
            self.binary_marker(&mut patch);
        }
        let metadata = patch.is_rename
            || patch.is_copy
            || patch.is_new != 0
            || patch.is_delete
            || (patch.old_mode != 0 && patch.new_mode != 0 && patch.old_mode != patch.new_mode);
        if patch.hunks.is_empty() && patch.binary == Binary::No && !metadata {
            diag!("error: patch with only garbage at line {start}");
            return Err(Fail(128));
        }
        Ok(Some(patch))
    }

    fn binary_marker(&mut self, patch: &mut FilePatch) {
        let line = self.peek();
        if line == b"GIT binary patch\n" {
            patch.binary = Binary::Data;
            self.advance();
        } else if line.starts_with(b"Binary files ") || line.starts_with(b"Files ") {
            patch.binary = Binary::Marker;
            self.advance();
        }
    }

    fn find_header(&mut self) -> Result<Option<FilePatch>, Fail> {
        while !self.done() {
            let line = self.peek();
            if line.len() < 6 {
                self.advance();
                continue;
            }
            if line.starts_with(b"@@ -") {
                diag!(
                    "error: patch fragment without header at line {}: {}",
                    self.line,
                    text(line)
                );
                return Err(Fail(128));
            }
            if let Some(rest) = line.strip_prefix(b"diff --git ") {
                let def = git_header_name(rest, self.strip);
                let (patch, consumed) = self.git_header(def)?;
                if consumed == 0 {
                    continue;
                }
                return Ok(Some(patch));
            }
            if line.starts_with(b"--- ") {
                let next = line_at(self.buf, self.pos + line.len());
                if next.starts_with(b"+++ ") {
                    return self.traditional().map(Some);
                }
            }
            self.advance();
        }
        Ok(None)
    }

    /// Reads the git extended header; returns the patch and how many lines past the first it consumed.
    fn git_header(&mut self, def_name: Option<Vec<u8>>) -> Result<(FilePatch, usize), Fail> {
        let mut p = FilePatch::new();
        p.git_header = true;
        let first_line = self.line;
        self.advance();
        let mut consumed = 0;
        loop {
            if self.done() {
                break;
            }
            let line = self.peek();
            if !line.ends_with(b"\n") {
                break;
            }
            let strip = self.strip;
            let line_no = self.line;
            if line.starts_with(b"@@ -") {
                break;
            } else if let Some(rest) = line.strip_prefix(b"--- ") {
                match header_name(rest, strip) {
                    Some(None) => {
                        p.old_name = None;
                        if !p.is_delete {
                            p.is_new = 1;
                        }
                    }
                    Some(Some(name)) => {
                        if let Some(prev) = &p.old_name
                            && *prev != name
                        {
                            diag!(
                                "error: git apply: bad git-diff - inconsistent old filename on line {line_no}"
                            );
                            return Err(Fail(128));
                        }
                        p.old_name = Some(name);
                    }
                    None => {
                        diag!(
                            "error: git diff header lacks filename information when removing {strip} leading pathname components (line {line_no})"
                        );
                        return Err(Fail(128));
                    }
                }
            } else if let Some(rest) = line.strip_prefix(b"+++ ") {
                match header_name(rest, strip) {
                    Some(None) => {
                        p.new_name = None;
                        p.is_delete = true;
                    }
                    Some(Some(name)) => {
                        if let Some(prev) = &p.new_name
                            && *prev != name
                        {
                            diag!(
                                "error: git apply: bad git-diff - inconsistent new filename on line {line_no}"
                            );
                            return Err(Fail(128));
                        }
                        p.new_name = Some(name);
                    }
                    None => {
                        diag!(
                            "error: git diff header lacks filename information when removing {strip} leading pathname components (line {line_no})"
                        );
                        return Err(Fail(128));
                    }
                }
            } else if let Some(rest) = line.strip_prefix(b"old mode ") {
                p.old_mode = octal(rest);
            } else if let Some(rest) = line.strip_prefix(b"new mode ") {
                p.new_mode = octal(rest);
            } else if let Some(rest) = line.strip_prefix(b"deleted file mode ") {
                p.is_delete = true;
                p.old_mode = octal(rest);
                p.new_name = None;
            } else if let Some(rest) = line.strip_prefix(b"new file mode ") {
                p.is_new = 1;
                p.new_mode = octal(rest);
                p.old_name = None;
            } else if let Some(rest) = line
                .strip_prefix(b"copy from ")
                .or_else(|| line.strip_prefix(b"rename from "))
                .or_else(|| line.strip_prefix(b"rename old "))
            {
                p.is_copy = line.starts_with(b"copy");
                p.is_rename = !p.is_copy;
                p.old_name = header_name(rest, 0).flatten();
            } else if let Some(rest) = line
                .strip_prefix(b"copy to ")
                .or_else(|| line.strip_prefix(b"rename to "))
                .or_else(|| line.strip_prefix(b"rename new "))
            {
                p.is_copy = line.starts_with(b"copy");
                p.is_rename = !p.is_copy;
                p.new_name = header_name(rest, 0).flatten();
            } else if let Some(rest) = line.strip_prefix(b"similarity index ") {
                p.score = percent(rest);
            } else if let Some(rest) = line.strip_prefix(b"dissimilarity index ") {
                p.score = percent(rest);
            } else if let Some(rest) = line.strip_prefix(b"index ") {
                index_line(&mut p, rest);
            } else {
                break;
            }
            consumed += 1;
            self.advance();
        }
        if p.is_new > 0 {
            p.old_name = None;
        }
        if p.is_delete {
            p.new_name = None;
        }
        if p.old_name.is_none() && p.new_name.is_none() {
            let Some(name) = def_name else {
                diag!(
                    "error: git diff header lacks filename information when removing {} leading pathname components (line {first_line})",
                    self.strip
                );
                return Err(Fail(128));
            };
            if p.is_new > 0 {
                p.new_name = Some(name);
            } else if p.is_delete {
                p.old_name = Some(name);
            } else {
                p.old_name = Some(name.clone());
                p.new_name = Some(name);
            }
        } else {
            if p.new_name.is_none() && !p.is_delete {
                p.new_name.clone_from(&p.old_name);
            }
            if p.old_name.is_none() && p.is_new <= 0 && !p.is_delete {
                p.old_name.clone_from(&p.new_name);
            }
        }
        Ok((p, consumed))
    }

    fn traditional(&mut self) -> Result<FilePatch, Fail> {
        let mut p = FilePatch::new();
        let (first, second) = (self.peek(), line_at(self.buf, self.pos + self.peek().len()));
        let line_no = self.line;
        let strip = self.strip;
        let a = header_name(&first[4..], strip);
        let b = header_name(&second[4..], strip);
        match (a, b) {
            (Some(None), Some(Some(name))) => {
                p.is_new = 1;
                p.new_name = Some(name);
            }
            (Some(Some(name)), Some(None)) => {
                p.is_delete = true;
                p.old_name = Some(name);
            }
            (Some(Some(x)), Some(Some(y))) => {
                if x == y {
                    p.old_name = Some(x.clone());
                    p.new_name = Some(x);
                } else {
                    p.old_name = Some(x);
                    p.new_name = Some(y);
                    let existing = p.new_name.clone().is_some_and(|n| {
                        std::path::Path::new(&*String::from_utf8_lossy(&n)).exists()
                    });
                    if !existing {
                        p.new_name.clone_from(&p.old_name);
                    } else {
                        p.old_name.clone_from(&p.new_name);
                    }
                }
            }
            _ => {
                diag!("error: unable to find filename in patch at line {line_no}");
                return Err(Fail(128));
            }
        }
        self.advance();
        self.advance();
        Ok(p)
    }

    fn fragments(&mut self, patch: &mut FilePatch) -> Result<(), Fail> {
        while self.peek().starts_with(b"@@ -") {
            let hunk = self.hunk(patch)?;
            patch.added += hunk.lines.iter().filter(|(m, _)| *m == b'+').count();
            patch.deleted += hunk.lines.iter().filter(|(m, _)| *m == b'-').count();
            patch.hunks.push(hunk);
        }
        Ok(())
    }

    fn corrupt(&self) -> Fail {
        diag!("error: corrupt patch at line {}", self.line);
        Fail(128)
    }

    fn hunk(&mut self, patch: &FilePatch) -> Result<Hunk, Fail> {
        let header_line = self.line;
        let Some((oldpos, oldlines, newpos, newlines)) = hunk_header(self.peek()) else {
            return Err(self.corrupt());
        };
        self.advance();
        let (mut old_left, mut new_left) = (oldlines, newlines);
        let (mut leading, mut trailing, mut changed) = (0, 0, false);
        let mut lines: Vec<(u8, Vec<u8>)> = Vec::new();
        while old_left > 0 || new_left > 0 {
            if self.done() {
                return Err(self.corrupt());
            }
            let line = self.peek();
            if !line.ends_with(b"\n") {
                return Err(self.corrupt());
            }
            match line[0] {
                b'\n' | b' ' => {
                    if old_left == 0 || new_left == 0 {
                        return Err(self.corrupt());
                    }
                    old_left -= 1;
                    new_left -= 1;
                    if !changed {
                        leading += 1;
                    }
                    trailing += 1;
                    let body = if line[0] == b'\n' { line } else { &line[1..] };
                    lines.push((b' ', body.to_vec()));
                }
                b'-' => {
                    if old_left == 0 {
                        return Err(self.corrupt());
                    }
                    old_left -= 1;
                    trailing = 0;
                    changed = true;
                    lines.push((b'-', line[1..].to_vec()));
                }
                b'+' => {
                    if new_left == 0 {
                        return Err(self.corrupt());
                    }
                    if self.ws {
                        let reported =
                            check_added(&line[1..], self.label, self.line, self.ws_errors);
                        self.ws_errors += usize::from(reported);
                    }
                    new_left -= 1;
                    trailing = 0;
                    changed = true;
                    lines.push((b'+', line[1..].to_vec()));
                }
                b'\\' => {
                    if line.len() < 12 || !line.starts_with(b"\\ ") {
                        return Err(self.corrupt());
                    }
                    no_newline(&mut lines);
                }
                _ => return Err(self.corrupt()),
            }
            self.advance();
        }
        if !changed {
            return Err(self.corrupt());
        }
        let marker = self.peek();
        if marker.len() > 12 && marker.starts_with(b"\\ ") {
            no_newline(&mut lines);
            self.advance();
        }
        if patch.is_new > 0 && old_left > 0 {
            diag!("error: new file depends on old contents");
            return Err(Fail(128));
        }
        Ok(Hunk {
            oldpos,
            oldlines,
            newpos,
            newlines,
            leading,
            trailing,
            lines,
            line: header_line,
        })
    }
}

/// A `\ No newline at end of file` line takes the newline off the line before it.
fn no_newline(lines: &mut [(u8, Vec<u8>)]) {
    if let Some((_, bytes)) = lines.last_mut()
        && bytes.ends_with(b"\n")
    {
        bytes.pop();
    }
}

fn percent(rest: &[u8]) -> u32 {
    let digits: Vec<u8> = rest
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    String::from_utf8_lossy(&digits).parse().unwrap_or(0)
}

fn index_line(p: &mut FilePatch, rest: &[u8]) {
    let rest = rest.strip_suffix(b"\n").unwrap_or(rest);
    let Some(dots) = rest.windows(2).position(|w| w == b"..") else {
        return;
    };
    let old = String::from_utf8_lossy(&rest[..dots]).into_owned();
    let tail = &rest[dots + 2..];
    let end = tail.iter().position(|b| *b == b' ').unwrap_or(tail.len());
    let new = String::from_utf8_lossy(&tail[..end]).into_owned();
    if old.bytes().all(|b| b == b'0') {
        p.old_id = None;
    } else {
        p.old_id = Some(old);
    }
    if !new.bytes().all(|b| b == b'0') {
        p.new_id = Some(new);
    }
    if end < tail.len() {
        p.old_mode = octal(&tail[end + 1..]);
    }
}

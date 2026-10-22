//! Line diff (Myers' O(ND) algorithm) and unified-diff rendering. No I/O and no dependencies.

use std::fmt::Write;

/// Past this many edits per file the middle is reported as one replace instead of a minimal diff, which bounds memory.
pub const MAX_EDITS: usize = 2000;

/// Bytes checked for a NUL when deciding that content is binary, as git does.
const BINARY_PROBE: usize = 8000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Equal,
    Delete,
    Insert,
}

pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_PROBE)].contains(&0)
}

/// Splits into lines, each keeping its trailing `\n`; the last may lack one.
pub fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|b| *b == b'\n').collect()
}

/// A shortest edit script turning `a` into `b`: one op per element consumed or produced, in order.
pub fn diff<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Op> {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[prefix..], &b[prefix..]);
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a, b) = (&a[..a.len() - suffix], &b[..b.len() - suffix]);

    let mut ops = vec![Op::Equal; prefix];
    match myers(a, b) {
        Some(mid) => ops.extend(mid),
        None => {
            ops.extend(std::iter::repeat_n(Op::Delete, a.len()));
            ops.extend(std::iter::repeat_n(Op::Insert, b.len()));
        }
    }
    ops.extend(std::iter::repeat_n(Op::Equal, suffix));
    ops
}

fn myers<T: PartialEq>(a: &[T], b: &[T]) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    if n == 0 || m == 0 {
        let mut ops = vec![Op::Delete; a.len()];
        ops.extend(std::iter::repeat_n(Op::Insert, b.len()));
        return Some(ops);
    }
    let cap = (n + m).min(MAX_EDITS as isize);
    let off = cap + 1;
    let mut v = vec![0isize; (2 * cap + 3) as usize];
    // trace[d] holds v[-d..=d] after round d, indexed by k + d.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let at = |k: isize| (k + off) as usize;
    let mut found = None;
    'rounds: for d in 0..=cap {
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                trace.push(v[at(-d)..=at(d)].to_vec());
                found = Some(d);
                break 'rounds;
            }
            k += 2;
        }
        trace.push(v[at(-d)..=at(d)].to_vec());
    }
    let d_total = found?;

    let (mut x, mut y) = (n, m);
    let mut rev: Vec<Op> = Vec::new();
    for d in (1..=d_total).rev() {
        let k = x - y;
        let prev = &trace[(d - 1) as usize];
        let get = |k: isize| prev[(k + d - 1) as usize];
        let prev_k = if k == -d || (k != d && get(k - 1) < get(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let (px, py) = (get(prev_k), get(prev_k) - prev_k);
        let (mx, my) = if prev_k == k + 1 {
            (px, py + 1)
        } else {
            (px + 1, py)
        };
        while x > mx && y > my {
            rev.push(Op::Equal);
            x -= 1;
            y -= 1;
        }
        rev.push(if prev_k == k + 1 {
            Op::Insert
        } else {
            Op::Delete
        });
        (x, y) = (px, py);
    }
    rev.extend(std::iter::repeat_n(Op::Equal, x.min(y) as usize));
    rev.reverse();
    Some(rev)
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// The edit script between two files' lines; with `ignore_ws`, lines equal once all whitespace is removed match.
fn line_ops(a: &[&[u8]], b: &[&[u8]], ignore_ws: bool) -> Vec<Op> {
    if !ignore_ws {
        return diff(a, b);
    }
    let key = |lines: &[&[u8]]| -> Vec<Vec<u8>> {
        lines
            .iter()
            .map(|l| l.iter().copied().filter(|c| !is_space(*c)).collect())
            .collect()
    };
    diff(&key(a), &key(b))
}

/// Lines added and deleted going from `old` to `new`.
pub fn line_counts(old: &[u8], new: &[u8], ignore_ws: bool) -> (usize, usize) {
    let ops = line_ops(&split_lines(old), &split_lines(new), ignore_ws);
    let added = ops.iter().filter(|o| **o == Op::Insert).count();
    let deleted = ops.iter().filter(|o| **o == Op::Delete).count();
    (added, deleted)
}

fn range(start: usize, len: usize) -> String {
    match len {
        1 => format!("{start}"),
        0 => format!("{},0", start.saturating_sub(1)),
        _ => format!("{start},{len}"),
    }
}

fn push_line(out: &mut String, tag: char, line: &[u8]) {
    out.push(tag);
    out.push_str(&String::from_utf8_lossy(line));
    if !line.ends_with(b"\n") {
        out.push_str("\n\\ No newline at end of file\n");
    }
}

const HEADING_BYTES: usize = 80;

/// git's default section heading: the nearest line above `start` (down to, not including, `floor`) that begins
/// with a letter, `_` or `$`.
fn heading(a: &[&[u8]], start: usize, floor: isize) -> String {
    let mut at = start as isize - 1;
    while at > floor {
        let line = a[at as usize];
        if line
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b'$')
        {
            let text = String::from_utf8_lossy(line.trim_ascii_end()).into_owned();
            let mut end = text.len().min(HEADING_BYTES);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            return format!(" {}", &text[..end]);
        }
        at -= 1;
    }
    String::new()
}

/// One hunk of a unified diff: its position on both sides and its lines in order.
/// A side's start is the line its first line sits on; with no lines on that side it is the line after the gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    /// The section heading shown after the `@@` header, with its leading space.
    pub heading: String,
    /// Each line keeps its newline; the last on a side may lack one.
    pub lines: Vec<(Op, Vec<u8>)>,
}

impl Hunk {
    pub fn header(&self) -> String {
        format!(
            "@@ -{} +{} @@{}\n",
            range(self.old_start, self.old_count),
            range(self.new_start, self.new_count),
            self.heading
        )
    }

    /// The header and the lines as `diff -u` prints them.
    pub fn render(&self) -> String {
        let mut out = self.header();
        for (op, line) in &self.lines {
            push_line(&mut out, tag(*op), line);
        }
        out
    }

    pub fn recount(&mut self) {
        let side = |keep: Op| {
            self.lines
                .iter()
                .filter(|(op, _)| *op == Op::Equal || *op == keep)
                .count()
        };
        (self.old_count, self.new_count) = (side(Op::Delete), side(Op::Insert));
    }

    /// The hunk that undoes this one.
    pub fn reversed(&self) -> Hunk {
        Hunk {
            old_start: self.new_start,
            old_count: self.new_count,
            new_start: self.old_start,
            new_count: self.old_count,
            heading: self.heading.clone(),
            lines: self
                .lines
                .iter()
                .map(|(op, line)| {
                    let op = match op {
                        Op::Equal => Op::Equal,
                        Op::Delete => Op::Insert,
                        Op::Insert => Op::Delete,
                    };
                    (op, line.clone())
                })
                .collect(),
        }
    }

    fn blocks(&self) -> Vec<(usize, usize)> {
        let mut blocks: Vec<(usize, usize)> = Vec::new();
        for (i, (op, _)) in self.lines.iter().enumerate() {
            match blocks.last_mut() {
                _ if *op == Op::Equal => {}
                Some((_, end)) if *end == i => *end = i + 1,
                _ => blocks.push((i, i + 1)),
            }
        }
        blocks
    }

    /// How many hunks `split` makes: the runs of changes with context between them.
    pub fn splittable_into(&self) -> usize {
        self.blocks().len().max(1)
    }

    /// One hunk per run of changes; the context between two runs belongs to both.
    pub fn split(&self) -> Vec<Hunk> {
        let blocks = self.blocks();
        if blocks.len() < 2 {
            return vec![self.clone()];
        }
        let mut out = Vec::new();
        for (n, (_, block_end)) in blocks.iter().enumerate() {
            let from = if n == 0 { 0 } else { blocks[n - 1].1 };
            let to = blocks.get(n + 1).map_or(self.lines.len(), |b| b.0);
            let before = &self.lines[..from];
            let count = |lines: &[(Op, Vec<u8>)], keep: Op| {
                lines
                    .iter()
                    .filter(|(op, _)| *op == Op::Equal || *op == keep)
                    .count()
            };
            let mut hunk = Hunk {
                old_start: self.old_start + count(before, Op::Delete),
                old_count: 0,
                new_start: self.new_start + count(before, Op::Insert),
                new_count: 0,
                heading: if n == 0 {
                    self.heading.clone()
                } else {
                    String::new()
                },
                lines: self.lines[from..to.max(*block_end)].to_vec(),
            };
            hunk.recount();
            out.push(hunk);
        }
        out
    }

    /// Reads a hunk back from text a person edited: the `@@` header and `#` lines are ignored, an empty line is
    /// context. `None` when no lines are left; the position stays that of `self`.
    pub fn from_edit(&self, text: &[u8]) -> Result<Option<Hunk>, String> {
        let mut lines: Vec<(Op, Vec<u8>)> = Vec::new();
        for raw in split_lines(text) {
            if raw.starts_with(b"#") || (lines.is_empty() && raw.starts_with(b"@@")) {
                continue;
            }
            let mut body = raw.to_vec();
            if !body.ends_with(b"\n") {
                body.push(b'\n');
            }
            let op = match body[0] {
                b'\\' => {
                    if let Some((_, last)) = lines.last_mut() {
                        last.pop();
                    }
                    continue;
                }
                b'\n' => {
                    lines.push((Op::Equal, body));
                    continue;
                }
                b' ' => Op::Equal,
                b'-' => Op::Delete,
                b'+' => Op::Insert,
                _ => {
                    return Err(format!(
                        "unrecognized line in the edited hunk: {}",
                        String::from_utf8_lossy(raw).trim_end()
                    ));
                }
            };
            lines.push((op, body[1..].to_vec()));
        }
        if lines.is_empty() {
            return Ok(None);
        }
        let mut hunk = Hunk {
            lines,
            heading: self.heading.clone(),
            ..self.clone()
        };
        hunk.recount();
        Ok(Some(hunk))
    }
}

fn tag(op: Op) -> char {
    match op {
        Op::Equal => ' ',
        Op::Delete => '-',
        Op::Insert => '+',
    }
}

/// Where a hunk would not go: the 1-based line it was meant for.
#[derive(Debug, PartialEq, Eq)]
pub struct Misfit(pub usize);

/// `content` with the hunks applied; each is placed at its line, or at the nearest place its old lines match.
pub fn apply_hunks(content: &[u8], hunks: &[&Hunk]) -> Result<Vec<u8>, Misfit> {
    let lines = split_lines(content);
    let mut ordered: Vec<&Hunk> = hunks.to_vec();
    ordered.sort_by_key(|h| h.old_start);
    let mut out: Vec<u8> = Vec::new();
    let mut done = 0;
    for hunk in ordered {
        let needle: Vec<&[u8]> = hunk
            .lines
            .iter()
            .filter(|(op, _)| *op != Op::Insert)
            .map(|(_, l)| l.as_slice())
            .collect();
        let expected = hunk.old_start.saturating_sub(1).min(lines.len());
        let start = place(&lines, &needle, expected).ok_or(Misfit(hunk.old_start))?;
        if start > done {
            lines[done..start]
                .iter()
                .for_each(|l| out.extend_from_slice(l));
            done = start;
        }
        let mut at = start;
        for (op, text) in &hunk.lines {
            match op {
                Op::Equal => {
                    if at >= done {
                        out.extend_from_slice(lines[at]);
                        done = at + 1;
                    }
                    at += 1;
                }
                Op::Delete => {
                    if at < done {
                        return Err(Misfit(hunk.old_start));
                    }
                    at += 1;
                    done = at;
                }
                Op::Insert => out.extend_from_slice(text),
            }
        }
    }
    lines[done.min(lines.len())..]
        .iter()
        .for_each(|l| out.extend_from_slice(l));
    Ok(out)
}

fn place(lines: &[&[u8]], needle: &[&[u8]], expected: usize) -> Option<usize> {
    let fits = |at: usize| lines.get(at..at + needle.len()) == Some(needle);
    if needle.is_empty() || fits(expected) {
        return Some(expected);
    }
    (1..=lines.len()).find_map(|d| {
        [expected.checked_sub(d), Some(expected + d)]
            .into_iter()
            .flatten()
            .find(|at| fits(*at))
    })
}

/// The hunks of a unified diff from `old` to `new` with `context` lines around each change; empty when the
/// contents are identical (or, with `ignore_ws`, differ only in whitespace). Context lines are those of `new`.
pub fn hunks(old: &[u8], new: &[u8], context: usize, ignore_ws: bool) -> Vec<Hunk> {
    let (a, b) = (split_lines(old), split_lines(new));
    let ops = line_ops(&a, &b, ignore_ws);
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| **o != Op::Equal)
        .map(|(i, _)| i)
        .collect();

    // Each group is a run of changes whose gaps are at most twice the context.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for &i in &changed {
        match groups.last_mut() {
            Some((_, end)) if i - *end <= 2 * context + 1 => *end = i,
            _ => groups.push((i, i)),
        }
    }

    let mut out = Vec::new();
    let mut floor = -1isize;
    for (first, last) in groups {
        let start = first.saturating_sub(context);
        let end = (last + 1 + context).min(ops.len());
        let (mut ai, mut bi) = (0, 0);
        for op in &ops[..start] {
            match op {
                Op::Equal => (ai, bi) = (ai + 1, bi + 1),
                Op::Delete => ai += 1,
                Op::Insert => bi += 1,
            }
        }
        let (a0, b0) = (ai, bi);
        let mut lines = Vec::new();
        for op in &ops[start..end] {
            match op {
                Op::Equal => {
                    lines.push((Op::Equal, b[bi].to_vec()));
                    (ai, bi) = (ai + 1, bi + 1);
                }
                Op::Delete => {
                    lines.push((Op::Delete, a[ai].to_vec()));
                    ai += 1;
                }
                Op::Insert => {
                    lines.push((Op::Insert, b[bi].to_vec()));
                    bi += 1;
                }
            }
        }
        out.push(Hunk {
            old_start: a0 + 1,
            old_count: ai - a0,
            new_start: b0 + 1,
            new_count: bi - b0,
            heading: heading(&a, a0, floor),
            lines,
        });
        floor = a0 as isize - 1;
    }
    out
}

/// The `@@` hunks of a unified diff, as `hunks` finds them, in text; `None` when there are none.
pub fn unified_hunks(old: &[u8], new: &[u8], context: usize, ignore_ws: bool) -> Option<String> {
    let found = hunks(old, new, context, ignore_ws);
    (!found.is_empty()).then(|| found.iter().map(Hunk::render).collect())
}

/// Trailing whitespace and the first space-before-tab in a line's indent, as git's default whitespace rules find them.
fn line_errors(line: &[u8]) -> (bool, bool) {
    let text = line.strip_suffix(b"\n").unwrap_or(line);
    let end = text
        .iter()
        .rposition(|c| !is_space(*c))
        .map_or(0, |i| i + 1);
    let mut written = 0;
    let mut space_before_tab = false;
    for (i, c) in text[..end].iter().enumerate() {
        match c {
            b' ' => {}
            b'\t' => {
                space_before_tab |= written < i;
                written = i + 1;
            }
            _ => break,
        }
    }
    (end < text.len(), space_before_tab)
}

fn trailing_blank_lines(lines: &[&[u8]]) -> usize {
    lines
        .iter()
        .rev()
        .take_while(|l| l.iter().all(|c| is_space(*c)))
        .count()
}

/// `git diff --check` for one file: a `path:line: <problem>.` line, then the offending added line, for trailing
/// whitespace and space before tab; blank lines added at the end of the file are reported last. Empty when clean.
pub fn whitespace_report(path: &str, old: &[u8], new: &[u8]) -> String {
    let (a, b) = (split_lines(old), split_lines(new));
    let mut out = String::new();
    let mut bi = 0;
    for op in diff(&a, &b) {
        match op {
            Op::Equal => bi += 1,
            Op::Delete => {}
            Op::Insert => {
                let (line_no, line) = (bi + 1, b[bi]);
                bi += 1;
                let (trailing, before_tab) = line_errors(line);
                let problems: Vec<&str> = [
                    (trailing, "trailing whitespace"),
                    (before_tab, "space before tab in indent"),
                ]
                .iter()
                .filter(|(bad, _)| *bad)
                .map(|(_, text)| *text)
                .collect();
                if !problems.is_empty() {
                    let _ = writeln!(out, "{path}:{line_no}: {}.", problems.join(", "));
                    push_line_plain(&mut out, line);
                }
            }
        }
    }
    let (was, now) = (trailing_blank_lines(&a), trailing_blank_lines(&b));
    if now > was {
        let _ = writeln!(out, "{path}:{}: new blank line at EOF.", b.len() - now + 1);
    }
    out
}

fn push_line_plain(out: &mut String, line: &[u8]) {
    out.push('+');
    out.push_str(&String::from_utf8_lossy(line));
    if !line.ends_with(b"\n") {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(a: &[&str], b: &[&str], ops: &[Op]) -> Vec<String> {
        let (mut i, mut j, mut out) = (0, 0, Vec::new());
        for op in ops {
            match op {
                Op::Equal => {
                    assert_eq!(a[i], b[j]);
                    out.push(a[i].to_string());
                    i += 1;
                    j += 1;
                }
                Op::Delete => i += 1,
                Op::Insert => {
                    out.push(b[j].to_string());
                    j += 1;
                }
            }
        }
        assert_eq!((i, j), (a.len(), b.len()));
        out
    }

    fn edits(ops: &[Op]) -> usize {
        ops.iter().filter(|o| **o != Op::Equal).count()
    }

    #[test]
    fn classic_example_is_minimal() {
        let a = ["A", "B", "C", "A", "B", "B", "A"];
        let b = ["C", "B", "A", "B", "A", "C"];
        let ops = diff(&a, &b);
        assert_eq!(edits(&ops), 5);
        assert_eq!(apply(&a, &b, &ops), b);
    }

    #[test]
    fn empty_and_identical_inputs() {
        let a: [&str; 0] = [];
        assert!(diff(&a, &a).is_empty());
        assert_eq!(diff(&a, &["x"]), vec![Op::Insert]);
        assert_eq!(diff(&["x"], &a), vec![Op::Delete]);
        assert_eq!(diff(&["x", "y"], &["x", "y"]), vec![Op::Equal; 2]);
    }

    #[test]
    fn random_inputs_reproduce_the_target() {
        let mut seed = 12345u64;
        let mut next = move |m: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % m
        };
        let alphabet = ["a", "b", "c", "d"];
        for _ in 0..300 {
            let a: Vec<&str> = (0..next(30)).map(|_| alphabet[next(4) as usize]).collect();
            let b: Vec<&str> = (0..next(30)).map(|_| alphabet[next(4) as usize]).collect();
            let ops = diff(&a, &b);
            assert_eq!(apply(&a, &b, &ops), b);
        }
    }

    #[test]
    fn a_huge_difference_still_terminates_and_reproduces() {
        let a: Vec<u32> = (0..5000).collect();
        let b: Vec<u32> = (5000..10000).collect();
        let ops = diff(&a, &b);
        assert_eq!(ops.iter().filter(|o| **o == Op::Delete).count(), 5000);
        assert_eq!(ops.iter().filter(|o| **o == Op::Insert).count(), 5000);
    }

    #[test]
    fn unified_output_matches_git_shape() {
        let old = b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
        let new = b"one\ntwo\nthree\nfour\nFIVE\nsix\nseven\neight\nnine\nten\n";
        let got = unified_hunks(old, new, 3, false).unwrap();
        assert_eq!(
            got,
            "@@ -2,7 +2,7 @@ one\n two\n three\n four\n-five\n+FIVE\n six\n seven\n eight\n"
        );
    }

    #[test]
    fn hunk_headings_are_the_nearest_letter_line_above_and_stop_at_the_previous_hunk() {
        let text = |lines: &[&str]| -> Vec<u8> {
            lines
                .iter()
                .flat_map(|l| format!("{l}\n").into_bytes())
                .collect()
        };
        let numbered: Vec<String> = (1..=20).map(|i| format!(" l{i}")).collect();
        let mut old = vec!["fn a"];
        old.extend(numbered.iter().map(String::as_str));
        let heads = |new: &[&str]| -> Vec<String> {
            let got = unified_hunks(&text(&old), &text(new), 1, false).unwrap();
            got.lines()
                .filter(|l| l.starts_with("@@"))
                .map(String::from)
                .collect()
        };
        let mut one = old.clone();
        one[19] = " y";
        assert_eq!(heads(&one), ["@@ -19,3 +19,3 @@ fn a"]);
        let mut two = one.clone();
        two[2] = " y";
        assert_eq!(heads(&two), ["@@ -2,3 +2,3 @@ fn a", "@@ -19,3 +19,3 @@"]);
    }

    #[test]
    fn distant_changes_make_separate_hunks_and_near_ones_merge() {
        let old: String = (1..=30).map(|i| format!("{i}\n")).collect();
        let new = old
            .replace("\n2\n", "\ntwo\n")
            .replace("\n29\n", "\ntwenty-nine\n");
        let got = unified_hunks(old.as_bytes(), new.as_bytes(), 3, false).unwrap();
        assert_eq!(got.matches("@@ -").count(), 2);
        let near = old
            .replace("\n2\n", "\ntwo\n")
            .replace("\n8\n", "\neight\n");
        let got = unified_hunks(old.as_bytes(), near.as_bytes(), 3, false).unwrap();
        assert_eq!(got.matches("@@ -").count(), 1);
    }

    #[test]
    fn identical_content_has_no_hunks() {
        assert!(unified_hunks(b"a\nb\n", b"a\nb\n", 3, false).is_none());
        assert!(unified_hunks(b"", b"", 3, false).is_none());
    }

    #[test]
    fn creation_and_deletion_use_zero_ranges() {
        assert_eq!(
            unified_hunks(b"", b"x\ny\n", 3, false).unwrap(),
            "@@ -0,0 +1,2 @@\n+x\n+y\n"
        );
        assert_eq!(
            unified_hunks(b"x\n", b"", 3, false).unwrap(),
            "@@ -1 +0,0 @@\n-x\n"
        );
    }

    #[test]
    fn missing_final_newline_is_marked() {
        let got = unified_hunks(b"a\nb", b"a\nb\n", 3, false).unwrap();
        assert_eq!(
            got,
            "@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n"
        );
    }

    fn numbered(edit: &[(usize, &str)]) -> (Vec<u8>, Vec<u8>) {
        let old: Vec<String> = (1..=20).map(|i| format!("{i}\n")).collect();
        let mut new = old.clone();
        for (i, text) in edit {
            new[*i - 1] = format!("{text}\n");
        }
        (old.concat().into_bytes(), new.concat().into_bytes())
    }

    #[test]
    fn a_hunk_splits_at_its_context_and_shares_it() {
        let (old, new) = numbered(&[(5, "five"), (7, "seven"), (11, "eleven")]);
        let whole = hunks(&old, &new, 3, false);
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].splittable_into(), 3);
        let parts = whole[0].split();
        let heads: Vec<String> = parts.iter().map(Hunk::header).collect();
        assert_eq!(
            heads,
            [
                "@@ -2,5 +2,5 @@\n",
                "@@ -6,5 +6,5 @@\n",
                "@@ -8,7 +8,7 @@\n"
            ]
        );
        assert_eq!(parts[1].splittable_into(), 1);
    }

    #[test]
    fn chosen_hunks_apply_on_their_own_and_together() {
        let (old, new) = numbered(&[(5, "five"), (7, "seven"), (11, "eleven")]);
        let parts = hunks(&old, &new, 3, false)[0].split();
        let both = apply_hunks(&old, &[&parts[0], &parts[1]]).unwrap();
        let (_, expect) = numbered(&[(5, "five"), (7, "seven")]);
        assert_eq!(both, expect);
        assert_eq!(apply_hunks(&old, &[]).unwrap(), old);
        let all: Vec<&Hunk> = parts.iter().collect();
        assert_eq!(apply_hunks(&old, &all).unwrap(), new);
    }

    #[test]
    fn a_reversed_hunk_takes_the_change_back() {
        let (old, new) = numbered(&[(5, "five"), (15, "fifteen")]);
        let found = hunks(&old, &new, 3, false);
        let back = found[1].reversed();
        let got = apply_hunks(&new, &[&back]).unwrap();
        let (_, expect) = numbered(&[(5, "five")]);
        assert_eq!(got, expect);
    }

    #[test]
    fn a_hunk_finds_its_lines_when_the_file_moved_them() {
        let (old, new) = numbered(&[(5, "five")]);
        let hunk = &hunks(&old, &new, 3, false)[0];
        let mut shifted = b"a\nb\n".to_vec();
        shifted.extend_from_slice(&old);
        let got = apply_hunks(&shifted, &[hunk]).unwrap();
        assert!(got.starts_with(b"a\nb\n1\n2\n3\n4\nfive\n"));
        assert_eq!(apply_hunks(b"x\ny\n", &[hunk]), Err(Misfit(2)));
    }

    #[test]
    fn an_edited_hunk_is_read_back_without_comments_and_header() {
        let (old, new) = numbered(&[(5, "five")]);
        let hunk = &hunks(&old, &new, 1, false)[0];
        let edited = b"# note\n@@ -4,3 +4,3 @@\n 4\n-5\n+FIVE\n\n\\ No newline at end of file\n";
        let got = hunk.from_edit(edited).unwrap().unwrap();
        assert_eq!(got.old_start, hunk.old_start);
        assert_eq!((got.old_count, got.new_count), (3, 3));
        assert_eq!(got.lines.last().unwrap().1, b"");
        assert!(hunk.from_edit(b"# nothing\n").unwrap().is_none());
        assert!(hunk.from_edit(b"?x\n").is_err());
    }

    #[test]
    fn nul_bytes_mean_binary() {
        assert!(is_binary(b"ab\0cd"));
        assert!(!is_binary("héllo\n".as_bytes()));
    }

    #[test]
    fn line_counts_count_changed_lines() {
        assert_eq!(line_counts(b"a\nb\nc\n", b"a\nX\nY\nc\n", false), (2, 1));
    }

    #[test]
    fn ignoring_whitespace_drops_whitespace_only_changes_and_prints_new_context() {
        assert!(unified_hunks(b"a b\n", b"ab \n", 3, true).is_none());
        assert!(unified_hunks(b"a\n", b"a", 3, true).is_none());
        assert_eq!(
            unified_hunks(b"a b\nx\n", b"ab\ny\n", 3, true).unwrap(),
            "@@ -1,2 +1,2 @@\n ab\n-x\n+y\n"
        );
        assert_eq!(line_counts(b"a b\nx\n", b"ab\ny\n", true), (1, 1));
        assert_eq!(line_counts(b"a b\nx\n", b"ab\ny\n", false), (2, 2));
    }

    #[test]
    fn whitespace_report_names_each_problem_in_git_order() {
        let new = b"a\n \tx  \n\t \ty\n \t\n  \tz\n\r\nc\n\n\n";
        assert_eq!(
            whitespace_report("f", b"a\n", new),
            "f:2: trailing whitespace, space before tab in indent.\n+ \tx  \n\
             f:3: space before tab in indent.\n+\t \ty\n\
             f:4: trailing whitespace.\n+ \t\n\
             f:5: space before tab in indent.\n+  \tz\n\
             f:6: trailing whitespace.\n+\r\n\
             f:8: new blank line at EOF.\n"
        );
    }

    #[test]
    fn whitespace_report_ignores_context_and_clean_additions() {
        assert_eq!(whitespace_report("f", b"a  \n", b"a  \nb\n"), "");
        assert_eq!(
            whitespace_report("f", b"a\n\n", b"a\n\n\n"),
            "f:2: new blank line at EOF.\n"
        );
        assert_eq!(whitespace_report("f", b"", b"x"), "");
    }
}

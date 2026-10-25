//! Line diff (Myers' O(ND) algorithm) and unified-diff rendering. No I/O and no dependencies.

use std::fmt::Write;

pub mod whitespace;

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

/// Which differences in whitespace make two lines count as equal, as git's `-w`, `-b`, `--ignore-space-at-eol`,
/// `--ignore-cr-at-eol` and `--ignore-blank-lines` do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ws {
    pub all: bool,
    pub change: bool,
    pub eol: bool,
    pub cr: bool,
    pub blank_lines: bool,
    /// `--minimal`: no shortcuts in finding the edit script.
    pub minimal: bool,
    /// `--no-indent-heuristic`: leave changed blocks where the diff first put them.
    pub no_indent_heuristic: bool,
}

impl Ws {
    pub fn all() -> Self {
        Self {
            all: true,
            ..Self::default()
        }
    }

    /// Whether lines are compared by something other than their exact bytes.
    pub fn compares_loosely(&self) -> bool {
        self.all || self.change || self.eol || self.cr
    }

    pub fn any(&self) -> bool {
        self.compares_loosely() || self.blank_lines
    }

    /// The bytes two lines are compared by.
    pub fn key(&self, line: &[u8]) -> Vec<u8> {
        if self.all {
            return line.iter().copied().filter(|c| !is_space(*c)).collect();
        }
        if self.change {
            let mut out = Vec::with_capacity(line.len());
            let mut spaced = false;
            for &c in line {
                if is_space(c) {
                    spaced = true;
                } else {
                    if spaced {
                        out.push(b' ');
                        spaced = false;
                    }
                    out.push(c);
                }
            }
            // A leading run still counts as one space.
            return out;
        }
        if self.eol {
            let end = line
                .iter()
                .rposition(|c| !is_space(*c))
                .map_or(0, |i| i + 1);
            return line[..end].to_vec();
        }
        if !self.cr {
            return line.to_vec();
        }
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        text.strip_suffix(b"\r").unwrap_or(text).to_vec()
    }
}

/// The edit script between two files' lines, comparing them as `ws` says.
fn line_ops(a: &[&[u8]], b: &[&[u8]], ws: &Ws) -> Vec<Op> {
    if !ws.compares_loosely() {
        return diff(a, b);
    }
    let key = |lines: &[&[u8]]| -> Vec<Vec<u8>> { lines.iter().map(|l| ws.key(l)).collect() };
    diff(&key(a), &key(b))
}

/// Lines added and deleted going from `old` to `new`.
pub fn line_counts(old: &[u8], new: &[u8], ignore_ws: bool) -> (usize, usize) {
    let ws = if ignore_ws { Ws::all() } else { Ws::default() };
    line_counts_with(old, new, &ws)
}

/// Lines added and deleted as git counts them under `ws`.
pub fn line_counts_with(old: &[u8], new: &[u8], ws: &Ws) -> (usize, usize) {
    if ws.blank_lines {
        let layout = Layout::default();
        let mut counts = (0, 0);
        for h in hunks_with(old, new, &layout, ws) {
            for (op, _) in &h.lines {
                match op {
                    Op::Insert => counts.0 += 1,
                    Op::Delete => counts.1 += 1,
                    Op::Equal => {}
                }
            }
        }
        return counts;
    }
    let ops = line_ops(&split_lines(old), &split_lines(new), ws);
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
/// with a letter, `_` or `$`; a hunk with none keeps the heading before it.
fn heading(a: &[&[u8]], start: usize, floor: isize) -> Option<String> {
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
            return Some(format!(" {}", &text[..end]));
        }
        at -= 1;
    }
    None
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

/// How hunks are cut: the context around changes, how far apart two changes may be and still share a hunk
/// (beyond twice the context), and whether a hunk grows to its whole function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub context: usize,
    pub inter_hunk: usize,
    pub function: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            context: 3,
            inter_hunk: 0,
            function: false,
        }
    }
}

/// One run of changed lines: where it starts on each side and how many lines it takes.
struct Change {
    i1: usize,
    chg1: usize,
    i2: usize,
    chg2: usize,
    ignore: bool,
}

fn changes_of(ops: &[Op]) -> Vec<Change> {
    let (mut i1, mut i2) = (0, 0);
    let mut out: Vec<Change> = Vec::new();
    let mut open = false;
    for op in ops {
        match op {
            Op::Equal => {
                open = false;
                (i1, i2) = (i1 + 1, i2 + 1);
            }
            _ => {
                if !open {
                    out.push(Change {
                        i1,
                        chg1: 0,
                        i2,
                        chg2: 0,
                        ignore: false,
                    });
                    open = true;
                }
                let c = out.last_mut().expect("a change was just opened");
                if *op == Op::Delete {
                    c.chg1 += 1;
                    i1 += 1;
                } else {
                    c.chg2 += 1;
                    i2 += 1;
                }
            }
        }
    }
    out
}

fn is_blank_record(line: &[u8], ws: &Ws) -> bool {
    if ws.compares_loosely() {
        line.iter().all(|c| is_space(*c))
    } else {
        line.len() <= 1
    }
}

/// The index in `changes` of the last change of the hunk starting at `first`, or `None` when only ignorable
/// changes are left; `first` moves past leading changes that are ignorable and far from the next.
fn next_hunk(changes: &[Change], first: &mut usize, layout: &Layout) -> Option<usize> {
    let max_common = 2 * layout.context + layout.inter_hunk;
    let max_ignorable = layout.context;
    let mut i = *first;
    while i < changes.len() && changes[i].ignore {
        let far = changes
            .get(i + 1)
            .is_none_or(|n| n.i1 - (changes[i].i1 + changes[i].chg1) >= max_ignorable);
        i += 1;
        if far {
            *first = i;
        }
    }
    if *first >= changes.len() {
        return None;
    }
    let mut last = *first;
    let mut ignored = 0;
    for at in *first + 1..changes.len() {
        let (prev, cur) = (&changes[at - 1], &changes[at]);
        let distance = cur.i1 - (prev.i1 + prev.chg1);
        if distance > max_common {
            break;
        }
        if distance < max_ignorable && (!cur.ignore || last == at - 1) {
            last = at;
            ignored = 0;
        } else if distance < max_ignorable && cur.ignore {
            ignored += cur.chg2;
        } else if last != at - 1
            && cur.i1 + ignored - (changes[last].i1 + changes[last].chg1) > max_common
        {
            break;
        } else if !cur.ignore {
            last = at;
            ignored = 0;
        } else {
            ignored += cur.chg2;
        }
    }
    Some(last)
}

fn is_empty_record(line: &[u8]) -> bool {
    line.iter().all(|c| is_space(*c))
}

fn is_function_line(line: &[u8]) -> bool {
    line.first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b'$')
}

/// The nearest function line from `start` towards `limit` (exclusive); `limit` below `start` searches backwards.
fn function_line(a: &[&[u8]], start: isize, limit: isize) -> Option<usize> {
    let step = if start > limit { -1 } else { 1 };
    let mut l = start;
    while l != limit && l >= 0 && (l as usize) < a.len() {
        if is_function_line(a[l as usize]) {
            return Some(l as usize);
        }
        l += step;
    }
    None
}

/// The hunks of a unified diff from `old` to `new` with `context` lines around each change; empty when the
/// contents are identical (or, with `ignore_ws`, differ only in whitespace). Context lines are those of `new`.
pub fn hunks(old: &[u8], new: &[u8], context: usize, ignore_ws: bool) -> Vec<Hunk> {
    let layout = Layout {
        context,
        ..Layout::default()
    };
    let ws = if ignore_ws { Ws::all() } else { Ws::default() };
    hunks_with(old, new, &layout, &ws)
}

/// The hunks of `old` against `new` cut as `layout` says, comparing lines as `ws` says.
pub fn hunks_with(old: &[u8], new: &[u8], layout: &Layout, ws: &Ws) -> Vec<Hunk> {
    let ops = line_ops(&split_lines(old), &split_lines(new), ws);
    let script: Vec<Edit> = changes_of(&ops)
        .iter()
        .map(|c| (c.i1, c.chg1, c.i2, c.chg2))
        .collect();
    hunks_from_script(old, new, layout, ws, &script)
}

/// One run of changed lines from an edit script: start and length on the old side, then on the new side.
pub type Edit = (usize, usize, usize, usize);

/// The hunks for an edit script a diff engine found, cut as `layout` says.
pub fn hunks_from_script(
    old: &[u8],
    new: &[u8],
    layout: &Layout,
    ws: &Ws,
    script: &[Edit],
) -> Vec<Hunk> {
    let (a, b) = (split_lines(old), split_lines(new));
    let mut changes: Vec<Change> = script
        .iter()
        .map(|&(i1, chg1, i2, chg2)| Change {
            i1,
            chg1,
            i2,
            chg2,
            ignore: false,
        })
        .collect();
    if ws.blank_lines {
        for c in &mut changes {
            c.ignore = a[c.i1..c.i1 + c.chg1]
                .iter()
                .chain(&b[c.i2..c.i2 + c.chg2])
                .all(|l| is_blank_record(l, ws));
        }
    }
    let ctx = layout.context;
    let mut out = Vec::new();
    let mut floor = -1isize;
    let mut found = String::new();
    let mut first = 0;
    while first < changes.len() {
        let Some(mut last) = next_hunk(&changes, &mut first, layout) else {
            break;
        };
        let head = &changes[first];
        let (mut s1, mut s2) = (head.i1.saturating_sub(ctx), head.i2.saturating_sub(ctx));
        if layout.function {
            // An appended block needs no pre-context when a function starts in it.
            let appended = head.i1 >= a.len();
            let whole = appended
                && b[head.i2.min(b.len())..]
                    .iter()
                    .any(|l| is_function_line(l));
            if !whole {
                let at = if appended {
                    a.len() as isize - 1
                } else {
                    head.i1 as isize
                };
                let mut start = function_line(&a, at, -1).unwrap_or(0);
                while start > 0 && !is_empty_record(a[start - 1]) && !is_function_line(a[start - 1])
                {
                    start -= 1;
                }
                if start < s1 {
                    s2 -= s1 - start;
                    s1 = start;
                }
            }
        }
        let (mut e1, mut e2);
        loop {
            let tail = &changes[last];
            let lctx = ctx
                .min(a.len() - (tail.i1 + tail.chg1))
                .min(b.len() - (tail.i2 + tail.chg2));
            e1 = tail.i1 + tail.chg1 + lctx;
            e2 = tail.i2 + tail.chg2 + lctx;
            if !layout.function {
                break;
            }
            let mut fe1 = a.len();
            if let Some(f) = function_line(&a, (tail.i1 + tail.chg1) as isize, a.len() as isize) {
                fe1 = f;
                while fe1 > 0 && is_empty_record(a[fe1 - 1]) {
                    fe1 -= 1;
                }
            }
            if fe1 > e1 {
                e2 += fe1 - e1;
                e1 = fe1;
            }
            match changes.get(last + 1) {
                Some(next) => {
                    let l = next.i1.min(a.len().saturating_sub(1));
                    if l.saturating_sub(ctx) <= e1
                        || function_line(&a, l as isize, e1 as isize).is_none()
                    {
                        last += 1;
                        continue;
                    }
                    break;
                }
                None => break,
            }
        }
        let mut lines = Vec::new();
        let mut p2 = s2;
        for c in &changes[first..=last] {
            for l in &b[p2..c.i2] {
                lines.push((Op::Equal, l.to_vec()));
            }
            for l in &a[c.i1..c.i1 + c.chg1] {
                lines.push((Op::Delete, l.to_vec()));
            }
            for l in &b[c.i2..c.i2 + c.chg2] {
                lines.push((Op::Insert, l.to_vec()));
            }
            p2 = c.i2 + c.chg2;
        }
        for l in &b[p2..e2] {
            lines.push((Op::Equal, l.to_vec()));
        }
        if let Some(h) = heading(&a, s1, floor) {
            found = h;
        }
        out.push(Hunk {
            old_start: s1 + 1,
            old_count: e1 - s1,
            new_start: s2 + 1,
            new_count: e2 - s2,
            heading: found.clone(),
            lines,
        });
        floor = s1 as isize - 1;
        first = last + 1;
    }
    out
}

/// The `@@` hunks of a unified diff, as `hunks` finds them, in text; `None` when there are none.
pub fn unified_hunks(old: &[u8], new: &[u8], context: usize, ignore_ws: bool) -> Option<String> {
    let found = hunks(old, new, context, ignore_ws);
    (!found.is_empty()).then(|| found.iter().map(Hunk::render).collect())
}

/// Whether `line` (without its marker) opens with seven of one of `<`, `=`, `>` or `|` and a space or newline.
fn is_conflict_marker(line: &[u8]) -> bool {
    const SIZE: usize = 7;
    line.len() > SIZE
        && matches!(line[0], b'=' | b'>' | b'<' | b'|')
        && line[..SIZE].iter().all(|c| *c == line[0])
        && is_space(line[SIZE])
}

/// `git diff --check` for one file: a `path:line: <problem>.` line, then the offending added line, for the
/// problems `rule` names; blank lines added at the end of the file are reported last. Empty when clean.
pub fn check_report(
    path: &str,
    (old, new): (&[u8], &[u8]),
    edits: &[Edit],
    rule: &whitespace::Rule,
    paint: Option<&whitespace::Paint>,
) -> String {
    let b = split_lines(new);
    let mut out = String::new();
    let plain = whitespace::Paint {
        set: "",
        reset: "",
        ws: "",
    };
    let paint = paint.unwrap_or(&plain);
    for &(_, _, i2, chg2) in edits {
        for (at, line) in b.iter().enumerate().skip(i2).take(chg2) {
            let line_no = at + 1;
            if is_conflict_marker(line) {
                let _ = writeln!(out, "{path}:{line_no}: leftover conflict marker");
            }
            let bad = whitespace::check(line, rule);
            if bad == 0 {
                continue;
            }
            let _ = writeln!(out, "{path}:{line_no}: {}.", whitespace::describe(bad));
            let _ = write!(out, "{}+{}", paint.set, paint.reset);
            out.push_str(&String::from_utf8_lossy(&whitespace::emit(
                line, rule, paint,
            )));
            if !line.ends_with(b"\n") {
                out.push('\n');
            }
        }
    }
    if rule.bits & whitespace::BLANK_AT_EOF != 0
        && let Some((_, at)) = whitespace::blank_at_eof(old, new)
    {
        let _ = writeln!(out, "{path}:{at}: new blank line at EOF.");
    }
    out
}

/// `check_report` for the default rules, on this crate's own edit script.
pub fn whitespace_report(path: &str, old: &[u8], new: &[u8]) -> String {
    let ops = line_ops(&split_lines(old), &split_lines(new), &Ws::default());
    let edits: Vec<Edit> = changes_of(&ops)
        .iter()
        .map(|c| (c.i1, c.chg1, c.i2, c.chg2))
        .collect();
    check_report(path, (old, new), &edits, &whitespace::Rule::default(), None)
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
    fn hunk_headings_are_the_nearest_letter_line_above_and_carry_over_when_none_is_found() {
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
        assert_eq!(
            heads(&two),
            ["@@ -2,3 +2,3 @@ fn a", "@@ -19,3 +19,3 @@ fn a"]
        );
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

    #[test]
    fn whitespace_modes_compare_lines_by_their_key() {
        let key = |ws: Ws, line: &[u8]| ws.key(line);
        let change = Ws {
            change: true,
            ..Ws::default()
        };
        assert_eq!(key(change, b"  a 	 b  \n"), b" a b");
        let eol = Ws {
            eol: true,
            ..Ws::default()
        };
        assert_eq!(key(eol, b"a b \t\r\n"), b"a b");
        let cr = Ws {
            cr: true,
            ..Ws::default()
        };
        assert_eq!(key(cr, b"a \r\n"), b"a ");
        assert_eq!(key(Ws::all(), b" a\tb \n"), b"ab");
        assert_eq!(key(Ws::default(), b" a \n"), b" a \n");
    }

    #[test]
    fn function_context_grows_a_hunk_to_the_function() {
        let old = b"fn a\n x\n y\n z\n w\nfn b\n one\n";
        let new = b"fn a\n x\n y\n Z\n w\nfn b\n one\n";
        let plain = Layout {
            context: 1,
            ..Layout::default()
        };
        let whole = Layout {
            function: true,
            ..plain
        };
        let text = |l: &Layout| -> String {
            hunks_with(old, new, l, &Ws::default())
                .iter()
                .map(Hunk::render)
                .collect()
        };
        assert_eq!(text(&plain), "@@ -3,3 +3,3 @@ fn a\n  y\n- z\n+ Z\n  w\n");
        assert_eq!(
            text(&whole),
            "@@ -1,5 +1,5 @@\n fn a\n  x\n  y\n- z\n+ Z\n  w\n"
        );
    }

    #[test]
    fn changes_closer_than_the_inter_hunk_context_share_a_hunk() {
        let old: Vec<u8> = (1..=12)
            .flat_map(|i| format!("{i}\n").into_bytes())
            .collect();
        let new = String::from_utf8(old.clone())
            .unwrap()
            .replace("\n3\n", "\nX\n")
            .replace("\n9\n", "\nY\n");
        let count = |inter| {
            let layout = Layout {
                context: 0,
                inter_hunk: inter,
                function: false,
            };
            hunks_with(&old, new.as_bytes(), &layout, &Ws::default()).len()
        };
        assert_eq!((count(0), count(4), count(5)), (2, 2, 1));
    }

    #[test]
    fn blank_line_changes_are_dropped_unless_a_neighbour_changes() {
        let ws = Ws {
            blank_lines: true,
            ..Ws::default()
        };
        let layout = Layout {
            context: 1,
            ..Layout::default()
        };
        assert!(hunks_with(b"a\nb\n", b"a\n\nb\n", &layout, &ws).is_empty());
        let shown = hunks_with(b"a\nb\n", b"a\n\nB\n", &layout, &ws);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].lines.len(), 4);
    }

    #[test]
    fn check_report_follows_the_rule_and_finds_conflict_markers() {
        let tabs = whitespace::Rule::parse("tab-in-indent,-trailing-space").unwrap();
        let new = b"\tx  \n<<<<<<< ours\n";
        let edits = [(0, 0, 0, 2)];
        assert_eq!(
            check_report("f", (b"", new), &edits, &tabs, None),
            "f:1: tab in indent.\n+\tx  \nf:2: leftover conflict marker\n"
        );
    }
}

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

/// Lines added and deleted going from `old` to `new`.
pub fn line_counts(old: &[u8], new: &[u8]) -> (usize, usize) {
    let ops = diff(&split_lines(old), &split_lines(new));
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

/// The `@@` hunks of a unified diff from `old` to `new` with `context` lines around each change;
/// `None` when the contents are identical.
pub fn unified_hunks(old: &[u8], new: &[u8], context: usize) -> Option<String> {
    let (a, b) = (split_lines(old), split_lines(new));
    let ops = diff(&a, &b);
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| **o != Op::Equal)
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return None;
    }

    // Each group is a run of changes whose gaps are at most twice the context.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for &i in &changed {
        match groups.last_mut() {
            Some((_, end)) if i - *end <= 2 * context + 1 => *end = i,
            _ => groups.push((i, i)),
        }
    }

    let mut out = String::new();
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
        let mut body = String::new();
        for op in &ops[start..end] {
            match op {
                Op::Equal => {
                    push_line(&mut body, ' ', a[ai]);
                    (ai, bi) = (ai + 1, bi + 1);
                }
                Op::Delete => {
                    push_line(&mut body, '-', a[ai]);
                    ai += 1;
                }
                Op::Insert => {
                    push_line(&mut body, '+', b[bi]);
                    bi += 1;
                }
            }
        }
        let _ = writeln!(
            out,
            "@@ -{} +{} @@{}",
            range(a0 + 1, ai - a0),
            range(b0 + 1, bi - b0),
            heading(&a, a0, floor)
        );
        floor = a0 as isize - 1;
        out.push_str(&body);
    }
    Some(out)
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
        let got = unified_hunks(old, new, 3).unwrap();
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
            let got = unified_hunks(&text(&old), &text(new), 1).unwrap();
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
        let got = unified_hunks(old.as_bytes(), new.as_bytes(), 3).unwrap();
        assert_eq!(got.matches("@@ -").count(), 2);
        let near = old
            .replace("\n2\n", "\ntwo\n")
            .replace("\n8\n", "\neight\n");
        let got = unified_hunks(old.as_bytes(), near.as_bytes(), 3).unwrap();
        assert_eq!(got.matches("@@ -").count(), 1);
    }

    #[test]
    fn identical_content_has_no_hunks() {
        assert!(unified_hunks(b"a\nb\n", b"a\nb\n", 3).is_none());
        assert!(unified_hunks(b"", b"", 3).is_none());
    }

    #[test]
    fn creation_and_deletion_use_zero_ranges() {
        assert_eq!(
            unified_hunks(b"", b"x\ny\n", 3).unwrap(),
            "@@ -0,0 +1,2 @@\n+x\n+y\n"
        );
        assert_eq!(
            unified_hunks(b"x\n", b"", 3).unwrap(),
            "@@ -1 +0,0 @@\n-x\n"
        );
    }

    #[test]
    fn missing_final_newline_is_marked() {
        let got = unified_hunks(b"a\nb", b"a\nb\n", 3).unwrap();
        assert_eq!(
            got,
            "@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n"
        );
    }

    #[test]
    fn nul_bytes_mean_binary() {
        assert!(is_binary(b"ab\0cd"));
        assert!(!is_binary("héllo\n".as_bytes()));
    }

    #[test]
    fn line_counts_count_changed_lines() {
        assert_eq!(line_counts(b"a\nb\nc\n", b"a\nX\nY\nc\n"), (2, 1));
    }
}

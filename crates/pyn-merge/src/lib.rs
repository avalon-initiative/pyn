//! Three-way merge (diff3 over Myers diffs) of sequences and text. No I/O and no git.
//!
//! Changes from the two sides whose base ranges overlap or touch are merged only when they are
//! identical; otherwise they form a conflict, as in git.

use pyn_diff::{Op, diff, is_binary, split_lines};

/// A run of the merged sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk<T> {
    Resolved(Vec<T>),
    Conflict(Conflict<T>),
}

/// The three versions of a region that could not be merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict<T> {
    pub base: Vec<T>,
    pub ours: Vec<T>,
    pub theirs: Vec<T>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<'a> {
    Clean(Vec<u8>),
    Conflicted(Vec<Chunk<&'a [u8]>>),
    /// A binary file changed differently on both sides; the file is the unit of conflict.
    BinaryConflict,
}

/// A changed range: `base[b0..b1]` became `side[s0..s1]`.
#[derive(Debug, Clone, Copy)]
struct Region {
    b0: usize,
    b1: usize,
    s0: usize,
    s1: usize,
}

fn regions(ops: &[Op]) -> Vec<Region> {
    let (mut b, mut s) = (0, 0);
    let mut out: Vec<Region> = Vec::new();
    let mut open = false;
    for op in ops {
        if *op == Op::Equal {
            (b, s) = (b + 1, s + 1);
            open = false;
            continue;
        }
        if !open {
            out.push(Region {
                b0: b,
                b1: b,
                s0: s,
                s1: s,
            });
            open = true;
        }
        if *op == Op::Delete {
            b += 1;
        } else {
            s += 1;
        }
        let r = out.last_mut().expect("region opened above");
        (r.b1, r.s1) = (b, s);
    }
    out
}

/// The side's range matching base `[start, end)`, widened by the unchanged lines around the regions.
fn side_range(rs: &[Region], start: usize, end: usize) -> Option<(usize, usize)> {
    let (first, last) = (rs.first()?, rs.last()?);
    Some((first.s0 - (first.b0 - start), last.s1 + (end - last.b1)))
}

fn push_resolved<T: Clone>(out: &mut Vec<Chunk<T>>, lines: &[T]) {
    if lines.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(Chunk::Resolved(v)) => v.extend_from_slice(lines),
        _ => out.push(Chunk::Resolved(lines.to_vec())),
    }
}

/// Which side wins a conflicting hunk instead of leaving a conflict (`-X ours`, `-X theirs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Favor {
    Ours,
    Theirs,
}

/// Merges `ours` and `theirs`, both derived from `base`, into resolved runs and conflicts.
pub fn merge<T: PartialEq + Clone>(base: &[T], ours: &[T], theirs: &[T]) -> Vec<Chunk<T>> {
    merge_favoring(base, ours, theirs, None)
}

/// Where the unchanged `base[b]` sits in a side; an insertion exactly at `b` lies after it.
fn locate(rs: &[Region], b: usize) -> usize {
    let shift: isize = rs
        .iter()
        .filter(|r| r.b1 < b || (r.b1 == b && r.b0 < b))
        .map(|r| (r.s1 - r.s0) as isize - (r.b1 - r.b0) as isize)
        .sum();
    (b as isize + shift) as usize
}

/// As [`merge`], resolving each conflicting hunk to `favor`'s side when given. Unchanged runs come from `ours`.
pub fn merge_favoring<T: PartialEq + Clone>(
    base: &[T],
    ours: &[T],
    theirs: &[T],
    favor: Option<Favor>,
) -> Vec<Chunk<T>> {
    let (ro, rt) = (regions(&diff(base, ours)), regions(&diff(base, theirs)));
    let mut all: Vec<(bool, Region)> = ro
        .iter()
        .map(|r| (false, *r))
        .chain(rt.iter().map(|r| (true, *r)))
        .collect();
    all.sort_by_key(|(_, r)| (r.b0, r.b1));

    let mut out = Vec::new();
    let mut ours_pos = 0;
    let mut i = 0;
    while i < all.len() {
        let start = all[i].1.b0;
        let mut end = all[i].1.b1;
        let mut j = i;
        while j < all.len() && all[j].1.b0 <= end {
            end = end.max(all[j].1.b1);
            j += 1;
        }
        let pick = |theirs_side: bool| -> Vec<Region> {
            all[i..j]
                .iter()
                .filter(|(t, _)| *t == theirs_side)
                .map(|(_, r)| *r)
                .collect()
        };
        let (rs_ours, rs_theirs) = (pick(false), pick(true));

        push_resolved(&mut out, &ours[ours_pos..locate(&ro, start)]);
        ours_pos = locate(&ro, end);
        let (side_ours, side_theirs) = (
            side_range(&rs_ours, start, end),
            side_range(&rs_theirs, start, end),
        );
        if let Some((_, b)) = side_ours {
            ours_pos = b;
        }
        match (side_ours, side_theirs) {
            (Some((a, b)), None) => push_resolved(&mut out, &ours[a..b]),
            (None, Some((a, b))) => push_resolved(&mut out, &theirs[a..b]),
            (Some((oa, ob)), Some((ta, tb))) => {
                if ours[oa..ob] == theirs[ta..tb] {
                    push_resolved(&mut out, &ours[oa..ob]);
                } else {
                    match favor {
                        Some(Favor::Ours) => push_resolved(&mut out, &ours[oa..ob]),
                        Some(Favor::Theirs) => push_resolved(&mut out, &theirs[ta..tb]),
                        None => out.push(Chunk::Conflict(Conflict {
                            base: base[start..end].to_vec(),
                            ours: ours[oa..ob].to_vec(),
                            theirs: theirs[ta..tb].to_vec(),
                        })),
                    }
                }
            }
            (None, None) => unreachable!("a cluster has at least one region"),
        }
        i = j;
    }
    push_resolved(&mut out, &ours[ours_pos..]);
    out
}

/// Worst-case work to merge text: the lines of all three inputs times the diff edit budget. Zero when the merge needs
/// no diff (an input equals another).
pub fn merge_cost(base: &[u8], ours: &[u8], theirs: &[u8]) -> u64 {
    if ours == theirs || theirs == base || ours == base {
        return 0;
    }
    let lines = |b: &[u8]| b.iter().filter(|c| **c == b'\n').count() as u64 + 1;
    (lines(base) + lines(ours) + lines(theirs)).saturating_mul(pyn_diff::MAX_EDITS as u64)
}

/// Which whitespace differences between lines do not count (`-X ignore-*`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Space {
    pub all: bool,
    pub change: bool,
    pub at_eol: bool,
    pub cr_at_eol: bool,
}

impl Space {
    fn any(self) -> bool {
        self.all || self.change || self.at_eol || self.cr_at_eol
    }

    /// The line reduced to what still has to match.
    fn key(self, line: &[u8]) -> Vec<u8> {
        let is_space = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
        let mut line = line.to_vec();
        if self.cr_at_eol && line.ends_with(b"\r\n") {
            line.splice(line.len() - 2.., *b"\n");
        }
        if self.all {
            line.retain(|b| !is_space(b));
        } else if self.change || self.at_eol {
            line.truncate(line.iter().rposition(|b| !is_space(b)).map_or(0, |i| i + 1));
            if self.change {
                let mut out: Vec<u8> = Vec::with_capacity(line.len());
                for b in line {
                    if !is_space(&b) {
                        out.push(b);
                    } else if out.last() != Some(&b' ') {
                        out.push(b' ');
                    }
                }
                return out;
            }
        }
        line
    }
}

/// How to merge text: which side wins conflicts and which whitespace differences are ignored.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub favor: Option<Favor>,
    pub space: Space,
}

/// A line that equals another when their reduced forms do, and keeps its own bytes for output.
#[derive(Clone)]
struct Keyed<'a> {
    raw: &'a [u8],
    key: Vec<u8>,
}

impl PartialEq for Keyed<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

fn keyed<'a>(bytes: &'a [u8], space: Space) -> Vec<Keyed<'a>> {
    split_lines(bytes)
        .into_iter()
        .map(|raw| Keyed {
            raw,
            key: space.key(raw),
        })
        .collect()
}

/// Merges text line by line; any NUL in an input makes the whole file the unit of conflict.
pub fn merge_text<'a>(base: &'a [u8], ours: &'a [u8], theirs: &'a [u8]) -> Outcome<'a> {
    merge_text_with(base, ours, theirs, Options::default())
}

/// As [`merge_text`], with `options` applied.
pub fn merge_text_with<'a>(
    base: &'a [u8],
    ours: &'a [u8],
    theirs: &'a [u8],
    options: Options,
) -> Outcome<'a> {
    if ours == theirs || theirs == base {
        return Outcome::Clean(ours.to_vec());
    }
    if ours == base {
        return Outcome::Clean(theirs.to_vec());
    }
    if is_binary(base) || is_binary(ours) || is_binary(theirs) {
        return Outcome::BinaryConflict;
    }
    let chunks: Vec<Chunk<&[u8]>> = if options.space.any() {
        let space = options.space;
        let (base_lines, our_lines, their_lines) =
            (keyed(base, space), keyed(ours, space), keyed(theirs, space));
        // A side that only differs in ignored whitespace changed nothing: the other side is the result.
        if base_lines == our_lines {
            return Outcome::Clean(theirs.to_vec());
        }
        if base_lines == their_lines {
            return Outcome::Clean(ours.to_vec());
        }
        let merged = merge_favoring(&base_lines, &our_lines, &their_lines, options.favor);
        let raw = |lines: Vec<Keyed<'a>>| lines.into_iter().map(|l| l.raw).collect::<Vec<_>>();
        merged
            .into_iter()
            .map(|c| match c {
                Chunk::Resolved(lines) => Chunk::Resolved(raw(lines)),
                Chunk::Conflict(c) => Chunk::Conflict(Conflict {
                    base: raw(c.base),
                    ours: raw(c.ours),
                    theirs: raw(c.theirs),
                }),
            })
            .collect()
    } else {
        merge_favoring(
            &split_lines(base),
            &split_lines(ours),
            &split_lines(theirs),
            options.favor,
        )
    };
    if chunks.iter().any(|c| matches!(c, Chunk::Conflict(_))) {
        return Outcome::Conflicted(chunks);
    }
    let mut out = Vec::new();
    for chunk in &chunks {
        if let Chunk::Resolved(lines) = chunk {
            lines.iter().for_each(|l| out.extend_from_slice(l));
        }
    }
    Outcome::Clean(out)
}

fn push_side(out: &mut Vec<u8>, lines: &[&[u8]]) {
    lines.iter().for_each(|l| out.extend_from_slice(l));
    if lines.last().is_some_and(|l| !l.ends_with(b"\n")) {
        out.push(b'\n');
    }
}

/// Renders chunks with git's default two-sided conflict markers, without the base.
pub fn render_two_way(chunks: &[Chunk<&[u8]>], ours: &str, theirs: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in chunks {
        match chunk {
            Chunk::Resolved(lines) => lines.iter().for_each(|l| out.extend_from_slice(l)),
            Chunk::Conflict(c) => {
                out.extend_from_slice(format!("<<<<<<< {ours}\n").as_bytes());
                push_side(&mut out, &c.ours);
                out.extend_from_slice(b"=======\n");
                push_side(&mut out, &c.theirs);
                out.extend_from_slice(format!(">>>>>>> {theirs}\n").as_bytes());
            }
        }
    }
    out
}

/// Renders chunks with diff3-style conflict markers, labelled with the given names.
pub fn render_markers(chunks: &[Chunk<&[u8]>], ours: &str, base: &str, theirs: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in chunks {
        match chunk {
            Chunk::Resolved(lines) => lines.iter().for_each(|l| out.extend_from_slice(l)),
            Chunk::Conflict(c) => {
                out.extend_from_slice(format!("<<<<<<< {ours}\n").as_bytes());
                push_side(&mut out, &c.ours);
                out.extend_from_slice(format!("||||||| {base}\n").as_bytes());
                push_side(&mut out, &c.base);
                out.extend_from_slice(b"=======\n");
                push_side(&mut out, &c.theirs);
                out.extend_from_slice(format!(">>>>>>> {theirs}\n").as_bytes());
            }
        }
    }
    out
}

#[cfg(test)]
mod cost_tests {
    use super::merge_cost;

    #[test]
    fn a_merge_that_needs_no_diff_costs_nothing() {
        assert_eq!(merge_cost(b"a\n", b"b\n", b"b\n"), 0);
        assert_eq!(merge_cost(b"a\n", b"a\n", b"b\n"), 0);
        assert_eq!(merge_cost(b"a\n", b"b\n", b"a\n"), 0);
    }

    #[test]
    fn cost_grows_with_the_lines_of_all_three_inputs() {
        let small = merge_cost(b"a\n", b"b\n", b"c\n");
        let big = merge_cost(b"a\na\na\n", b"b\nb\nb\n", b"c\nc\nc\n");
        assert!(small > 0 && big > small);
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;

    fn clean(outcome: Outcome) -> String {
        match outcome {
            Outcome::Clean(bytes) => String::from_utf8(bytes).unwrap(),
            other => panic!("not clean: {other:?}"),
        }
    }

    const BASE: &[u8] = b"a\nb\nc\nd\ne\n";

    #[test]
    fn favor_settles_conflicting_hunks_and_keeps_the_rest_merged() {
        let (ours, theirs) = (
            b"a\nB1\nc\nd\ne\nours\n".as_slice(),
            b"a\nB2\nc\nd\ne\n".as_slice(),
        );
        assert!(matches!(
            merge_text(BASE, ours, theirs),
            Outcome::Conflicted(_)
        ));
        for (favor, want) in [
            (Favor::Ours, "a\nB1\nc\nd\ne\nours\n"),
            (Favor::Theirs, "a\nB2\nc\nd\ne\nours\n"),
        ] {
            let options = Options {
                favor: Some(favor),
                ..Options::default()
            };
            assert_eq!(clean(merge_text_with(BASE, ours, theirs, options)), want);
        }
    }

    #[test]
    fn a_side_that_changes_only_ignored_whitespace_changed_nothing() {
        let options = Options {
            space: Space {
                change: true,
                ..Space::default()
            },
            ..Options::default()
        };
        let base = b"a\nb b\nc\n";
        let (ours, theirs) = (b"A\nb  b\nc\n".as_slice(), b"a\nb b\nC\n".as_slice());
        assert_eq!(
            clean(merge_text_with(base, ours, theirs, options)),
            "A\nb  b\nC\n"
        );
        assert!(matches!(
            merge_text(base, ours, theirs),
            Outcome::Conflicted(_)
        ));
        let loose = b"a\nb   b\nc\n".as_slice();
        assert_eq!(
            clean(merge_text_with(base, ours, loose, options)),
            "A\nb  b\nc\n"
        );
        assert_eq!(
            clean(merge_text_with(base, loose, ours, options)),
            "A\nb  b\nc\n"
        );
        assert_eq!(
            clean(merge_text_with(base, loose, loose, options)),
            "a\nb   b\nc\n"
        );
    }

    #[test]
    fn whitespace_keys_follow_each_option() {
        let key = |space: Space, line: &str| String::from_utf8(space.key(line.as_bytes())).unwrap();
        let all = Space {
            all: true,
            ..Space::default()
        };
        let change = Space {
            change: true,
            ..Space::default()
        };
        let eol = Space {
            at_eol: true,
            ..Space::default()
        };
        let cr = Space {
            cr_at_eol: true,
            ..Space::default()
        };
        assert_eq!(key(all, " a \t b \n"), "ab");
        assert_eq!(key(change, " a \t b  \n"), " a b");
        assert_eq!(key(eol, " a  b \r\n"), " a  b");
        assert_eq!(key(cr, "a \r\n"), "a \n");
        assert_eq!(key(cr, "a\r"), "a\r");
    }

    #[test]
    fn unchanged_runs_are_ours_when_lines_only_match_loosely() {
        let space = Space {
            all: true,
            ..Space::default()
        };
        let (base, ours, theirs) = (
            keyed(b"x\ny\nz\n", space),
            keyed(b"x \ny\nZ\n", space),
            keyed(b"x\ny\nz\n", space),
        );
        let chunks = merge_favoring(&base, &ours, &theirs, None);
        let Chunk::Resolved(lines) = &chunks[0] else {
            panic!("resolved first")
        };
        assert_eq!(lines[0].raw, b"x \n");
    }
}

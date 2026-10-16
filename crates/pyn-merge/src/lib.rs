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

/// Merges `ours` and `theirs`, both derived from `base`, into resolved runs and conflicts.
pub fn merge<T: PartialEq + Clone>(base: &[T], ours: &[T], theirs: &[T]) -> Vec<Chunk<T>> {
    let mut all: Vec<(bool, Region)> = regions(&diff(base, ours))
        .into_iter()
        .map(|r| (false, r))
        .chain(regions(&diff(base, theirs)).into_iter().map(|r| (true, r)))
        .collect();
    all.sort_by_key(|(_, r)| (r.b0, r.b1));

    let mut out = Vec::new();
    let mut pos = 0;
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

        push_resolved(&mut out, &base[pos..start]);
        pos = end;
        match (
            side_range(&rs_ours, start, end),
            side_range(&rs_theirs, start, end),
        ) {
            (Some((a, b)), None) => push_resolved(&mut out, &ours[a..b]),
            (None, Some((a, b))) => push_resolved(&mut out, &theirs[a..b]),
            (Some((oa, ob)), Some((ta, tb))) => {
                if ours[oa..ob] == theirs[ta..tb] {
                    push_resolved(&mut out, &ours[oa..ob]);
                } else {
                    out.push(Chunk::Conflict(Conflict {
                        base: base[start..end].to_vec(),
                        ours: ours[oa..ob].to_vec(),
                        theirs: theirs[ta..tb].to_vec(),
                    }));
                }
            }
            (None, None) => unreachable!("a cluster has at least one region"),
        }
        i = j;
    }
    push_resolved(&mut out, &base[pos..]);
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

/// Merges text line by line; any NUL in an input makes the whole file the unit of conflict.
pub fn merge_text<'a>(base: &'a [u8], ours: &'a [u8], theirs: &'a [u8]) -> Outcome<'a> {
    if ours == theirs || theirs == base {
        return Outcome::Clean(ours.to_vec());
    }
    if ours == base {
        return Outcome::Clean(theirs.to_vec());
    }
    if is_binary(base) || is_binary(ours) || is_binary(theirs) {
        return Outcome::BinaryConflict;
    }
    let chunks = merge(&split_lines(base), &split_lines(ours), &split_lines(theirs));
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

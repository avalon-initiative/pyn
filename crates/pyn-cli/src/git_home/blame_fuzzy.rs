//! Guessing which parent line a changed line replaced, as `git blame --ignore-rev` does: lines are compared by their
//! lower-cased byte pairs, and the best matches that keep the line order win (a port of blame.c's fuzzy matcher).

use std::collections::HashMap;

/// The multiset of a line's byte pairs, whitespace folded to zero and padded at both ends.
pub(super) type Fingerprint = HashMap<u16, i64>;

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

pub(super) fn fingerprint(line: &[u8]) -> Fingerprint {
    let mut map = Fingerprint::new();
    let mut previous = 0u16;
    for p in 0..=line.len() {
        let c = if p == line.len() || is_space(line[p]) {
            0
        } else {
            u16::from(line[p].to_ascii_lowercase())
        };
        let pair = previous | (c << 8);
        previous = c;
        if pair != 0 {
            *map.entry(pair).or_insert(0) += 1;
        }
    }
    map
}

pub(super) fn fingerprints(lines: &[&[u8]]) -> Vec<Fingerprint> {
    lines.iter().map(|l| fingerprint(l)).collect()
}

fn similarity(a: &Fingerprint, b: &Fingerprint) -> i64 {
    b.iter()
        .map(|(k, n)| a.get(k).map_or(0, |m| (*m).min(*n)))
        .sum()
}

fn subtract(a: &mut Fingerprint, b: &Fingerprint) {
    for (k, n) in b {
        if let Some(m) = a.get_mut(k) {
            if *m <= *n {
                a.remove(k);
            } else {
                *m -= *n;
            }
        }
    }
}

const NOTHING_MATCHES: i64 = -2;
const NOT_CALCULATED: i64 = -1;

/// Maps a line of the target chunk onto the parent chunk by position, centring on the middle of its span.
struct Mapping {
    destination_start: i64,
    destination_length: i64,
    source_start: i64,
    source_length: i64,
}

impl Mapping {
    fn map(&self, line: i64) -> i64 {
        ((line - self.source_start) * 2 + 1) * self.destination_length / (self.source_length * 2)
            + self.destination_start
    }
}

struct Matcher {
    max_a: i64,
    max_b: i64,
    mapping: Mapping,
}

struct Rows<'a> {
    similarities: &'a mut [i64],
    certainties: &'a mut [i64],
    second_best: &'a mut [i64],
    result: &'a mut [i64],
}

impl Matcher {
    fn slot(&self, line_a: i64, local_b: i64, closest_a: i64) -> usize {
        (line_a - closest_a + self.max_a + local_b * (self.max_a * 2 + 1)) as usize
    }

    #[allow(clippy::too_many_arguments)]
    fn best_matches(
        &self,
        start_a: i64,
        length_a: i64,
        start_b: i64,
        local_b: i64,
        fa: &[Fingerprint],
        fb: &[Fingerprint],
        rows: &mut Rows,
    ) {
        if rows.certainties[local_b as usize] != NOT_CALCULATED {
            return;
        }
        let closest = self.mapping.map(local_b + start_b) - start_a;
        let search_start = (closest - self.max_a).max(0);
        let search_end = (closest + self.max_a + 1).min(length_a);
        let (mut best, mut second) = (0, 0);
        let (mut best_at, mut second_at) = (0, 0);
        for i in search_start..search_end {
            let at = self.slot(i, local_b, closest);
            if rows.similarities[at] == -1 {
                rows.similarities[at] = similarity(&fb[local_b as usize], &fa[i as usize])
                    * (1000 - (i - closest).abs());
            }
            let s = rows.similarities[at];
            if s > best {
                second = best;
                second_at = best_at;
                best = s;
                best_at = i;
            } else if s > second {
                second = s;
                second_at = i;
            }
        }
        let b = local_b as usize;
        if best == 0 {
            rows.certainties[b] = NOTHING_MATCHES;
            rows.result[b] = -1;
        } else {
            rows.certainties[b] = best * 2 - second;
            rows.result[b] = start_a + best_at;
            rows.second_best[b] = start_a + second_at;
        }
    }

    /// Takes the most certain line as a partition and recurses on both sides so the order is kept.
    #[allow(clippy::too_many_arguments)]
    fn recurse(
        &self,
        start_a: i64,
        start_b: i64,
        length_a: i64,
        length_b: i64,
        fa: &mut [Fingerprint],
        fb: &[Fingerprint],
        rows: &mut Rows,
    ) {
        let (mut most_local_b, mut most_certainty) = (-1i64, -1i64);
        for i in 0..length_b {
            self.best_matches(start_a, length_a, start_b, i, fa, fb, rows);
            if rows.certainties[i as usize] > most_certainty {
                most_certainty = rows.certainties[i as usize];
                most_local_b = i;
            }
        }
        if most_local_b == -1 {
            return;
        }
        let most_a = rows.result[most_local_b as usize];
        let fb_most = fb[most_local_b as usize].clone();
        subtract(&mut fa[(most_a - start_a) as usize], &fb_most);

        let invalidate_min = (most_local_b - self.max_b).max(0);
        let invalidate_max = (most_local_b + self.max_b + 1).min(length_b);
        for i in invalidate_min..invalidate_max {
            let closest = self.mapping.map(i + start_b) - start_a;
            if (most_a - start_a - closest).abs() > self.max_a {
                continue;
            }
            let at = self.slot(most_a - start_a, i, closest);
            rows.similarities[at] = -1;
        }
        for i in (invalidate_min..most_local_b).rev() {
            let u = i as usize;
            if rows.certainties[u] >= 0
                && (rows.result[u] >= most_a || rows.second_best[u] >= most_a)
            {
                rows.certainties[u] = NOT_CALCULATED;
            }
        }
        for i in most_local_b + 1..invalidate_max {
            let u = i as usize;
            if rows.certainties[u] >= 0
                && (rows.result[u] <= most_a || rows.second_best[u] <= most_a)
            {
                rows.certainties[u] = NOT_CALCULATED;
            }
        }

        if most_local_b > 0 {
            self.recurse(
                start_a,
                start_b,
                most_a + 1 - start_a,
                most_local_b,
                fa,
                fb,
                rows,
            );
        }
        if most_local_b + 1 < length_b {
            let offset_b = most_local_b + 1;
            let width = (self.max_a * 2 + 1) as usize;
            let mut tail = Rows {
                similarities: &mut rows.similarities[offset_b as usize * width..],
                certainties: &mut rows.certainties[offset_b as usize..],
                second_best: &mut rows.second_best[offset_b as usize..],
                result: &mut rows.result[offset_b as usize..],
            };
            self.recurse(
                most_a,
                start_b + offset_b,
                length_a + start_a - most_a,
                length_b + start_b - (start_b + offset_b),
                &mut fa[(most_a - start_a) as usize..],
                &fb[offset_b as usize..],
                &mut tail,
            );
        }
    }
}

/// For each target line of the chunk, the parent line it most resembles in the parent's chunk, or -1.
fn fuzzy_find(
    parent: &mut [Fingerprint],
    target: &[Fingerprint],
    tlno: i64,
    parent_slno: i64,
    same: i64,
    parent_len: i64,
) -> Option<Vec<i64>> {
    let (start_a, length_a, start_b, length_b) = (parent_slno, parent_len, tlno, same - tlno);
    if length_a <= 0 {
        return None;
    }
    let max_a = 10.min(length_a - 1);
    let max_b = ((2 * max_a + 1) * length_b - 1) / length_a;
    let matcher = Matcher {
        max_a,
        max_b,
        mapping: Mapping {
            destination_start: start_a,
            destination_length: length_a,
            source_start: start_b,
            source_length: length_b,
        },
    };
    let n = length_b as usize;
    let mut similarities = vec![-1i64; n * (max_a * 2 + 1) as usize];
    let mut certainties = vec![NOT_CALCULATED; n];
    let mut second_best = vec![-1i64; n];
    let mut result = vec![-1i64; n];
    let mut rows = Rows {
        similarities: &mut similarities,
        certainties: &mut certainties,
        second_best: &mut second_best,
        result: &mut result,
    };
    matcher.recurse(
        start_a,
        start_b,
        length_a,
        length_b,
        &mut parent[start_a as usize..],
        &target[start_b as usize..],
        &mut rows,
    );
    Some(result)
}

/// The parent line most similar to target line `t_idx` over the whole parent file, if one is similar enough.
fn scan_parent_range(parent: &[Fingerprint], target: &[Fingerprint], t_idx: i64) -> i64 {
    let mut best_sim = 10;
    let mut best_idx = -1i64;
    for (p_idx, fp) in parent.iter().enumerate() {
        let p_idx = p_idx as i64;
        let sim = similarity(&target[t_idx as usize], fp);
        if sim < best_sim {
            continue;
        }
        if sim == best_sim && best_idx != -1 && (best_idx - t_idx).abs() < (p_idx - t_idx).abs() {
            continue;
        }
        best_sim = sim;
        best_idx = p_idx;
    }
    best_idx
}

/// Per target line of a changed chunk: `Some(parent line)` when it is blamed on the parent, else `None`.
pub(super) fn guess_line_blames(
    parent: &mut [Fingerprint],
    target: &[Fingerprint],
    tlno: usize,
    parent_slno: usize,
    same: usize,
    parent_len: usize,
) -> Vec<Option<usize>> {
    let fuzzy = fuzzy_find(
        parent,
        target,
        tlno as i64,
        parent_slno as i64,
        same as i64,
        parent_len as i64,
    );
    (0..same - tlno)
        .map(|i| {
            let best = match &fuzzy {
                Some(m) if m[i] >= 0 => m[i],
                _ => scan_parent_range(parent, target, (tlno + i) as i64),
            };
            (best >= 0).then_some(best as usize)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_counts_shared_byte_pairs() {
        let (a, b) = (fingerprint(b"cat mat"), fingerprint(b"father rather"));
        assert_eq!(similarity(&a, &b), 2);
        assert_eq!(similarity(&fingerprint(b"tim"), &fingerprint(b"mit")), 0);
    }
}

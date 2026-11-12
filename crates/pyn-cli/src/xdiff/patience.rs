//! xdiff's patience diff (`xpatience.c`): anchor on the longest run of lines unique to both sides.

use super::{Side, fall_back, mark_changed};
use std::collections::HashMap;

struct Env<'s, 'a> {
    x: &'s mut Side<'a>,
    y: &'s mut Side<'a>,
    ws: &'s pyn_diff::Ws,
}

#[derive(Default)]
struct Entry {
    count1: usize,
    count2: usize,
    line1: usize,
    line2: usize,
}

pub(super) fn diff<'a>(x: &mut Side<'a>, y: &mut Side<'a>, ws: &pyn_diff::Ws) {
    let (n1, n2) = (x.recs.len(), y.recs.len());
    let mut env = Env { x, y, ws };
    patience(&mut env, (1, n1), (1, n2));
}

fn patience(env: &mut Env, (line1, count1): (usize, usize), (line2, count2): (usize, usize)) {
    if count1 == 0 {
        mark_changed(env.y, line2, count2);
        return;
    }
    if count2 == 0 {
        mark_changed(env.x, line1, count1);
        return;
    }
    let mut order: Vec<usize> = Vec::new();
    let mut map: HashMap<usize, Entry> = HashMap::new();
    for line in line1..line1 + count1 {
        let class = env.x.class[line - 1];
        let e = map.entry(class).or_insert_with(|| {
            order.push(class);
            Entry {
                line1: line,
                ..Entry::default()
            }
        });
        e.count1 += 1;
    }
    let mut has_matches = false;
    for line in line2..line2 + count2 {
        if let Some(e) = map.get_mut(&env.y.class[line - 1]) {
            has_matches = true;
            e.count2 += 1;
            e.line2 = line;
        }
    }
    if !has_matches {
        mark_changed(env.x, line1, count1);
        mark_changed(env.y, line2, count2);
        return;
    }
    let chain = longest_common_sequence(&order, &map);
    if chain.is_empty() {
        fall_back(env.x, env.y, env.ws, (line1, count1), (line2, count2));
    } else {
        walk(env, &chain, (line1, count1), (line2, count2));
    }
}

/// The longest increasing run of unique-in-both lines, as `(line1, line2)` pairs.
fn longest_common_sequence(order: &[usize], map: &HashMap<usize, Entry>) -> Vec<(usize, usize)> {
    let uniques: Vec<(usize, usize)> = order
        .iter()
        .map(|c| &map[c])
        .filter(|e| e.count1 == 1 && e.count2 == 1)
        .map(|e| (e.line1, e.line2))
        .collect();
    let mut sequence: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; uniques.len()];
    for (n, entry) in uniques.iter().enumerate() {
        let (mut left, mut right) = (-1isize, sequence.len() as isize);
        while left + 1 < right {
            let middle = left + (right - left) / 2;
            if uniques[sequence[middle as usize]].1 > entry.1 {
                right = middle;
            } else {
                left = middle;
            }
        }
        previous[n] = usize::try_from(left).ok().map(|i| sequence[i]);
        let slot = (left + 1) as usize;
        if slot == sequence.len() {
            sequence.push(n);
        } else {
            sequence[slot] = n;
        }
    }
    let mut chain = Vec::new();
    let mut at = sequence.last().copied();
    while let Some(n) = at {
        chain.push(uniques[n]);
        at = previous[n];
    }
    chain.reverse();
    chain
}

fn same(env: &Env, line1: usize, line2: usize) -> bool {
    env.x.class[line1 - 1] == env.y.class[line2 - 1]
}

fn walk(
    env: &mut Env,
    chain: &[(usize, usize)],
    (mut line1, count1): (usize, usize),
    (mut line2, count2): (usize, usize),
) {
    let (end1, end2) = (line1 + count1, line2 + count2);
    let mut at = Some(0);
    loop {
        let (mut next1, mut next2) = match at {
            Some(i) => chain[i],
            None => (end1, end2),
        };
        if at.is_some() {
            while next1 > line1 && next2 > line2 && same(env, next1 - 1, next2 - 1) {
                next1 -= 1;
                next2 -= 1;
            }
        }
        while line1 < next1 && line2 < next2 && same(env, line1, line2) {
            line1 += 1;
            line2 += 1;
        }
        if next1 > line1 || next2 > line2 {
            patience(env, (line1, next1 - line1), (line2, next2 - line2));
        }
        let Some(mut i) = at else { return };
        while i + 1 < chain.len()
            && chain[i + 1].0 == chain[i].0 + 1
            && chain[i + 1].1 == chain[i].1 + 1
        {
            i += 1;
        }
        line1 = chain[i].0 + 1;
        line2 = chain[i].1 + 1;
        at = (i + 1 < chain.len()).then_some(i + 1);
    }
}

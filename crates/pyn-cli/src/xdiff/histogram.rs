//! xdiff's histogram diff (`xhistogram.c`): anchor on the longest common run whose lines are the rarest.

use super::{Side, fall_back, mark_changed};
use std::collections::HashMap;

const MAX_CHAIN_LENGTH: usize = 64;

#[derive(Clone, Copy, Default)]
struct Region {
    begin1: usize,
    end1: usize,
    begin2: usize,
    end2: usize,
}

struct Index {
    first: HashMap<usize, usize>,
    count: HashMap<usize, usize>,
    next: Vec<usize>,
    shift: usize,
    cnt: usize,
    has_common: bool,
}

pub(super) fn diff<'a>(x: &mut Side<'a>, y: &mut Side<'a>, ws: &pyn_diff::Ws) {
    let (n1, n2) = (x.recs.len(), y.recs.len());
    histogram(x, y, ws, (1, n1), (1, n2));
}

fn histogram<'a>(
    x: &mut Side<'a>,
    y: &mut Side<'a>,
    ws: &pyn_diff::Ws,
    (mut line1, mut count1): (usize, usize),
    (mut line2, mut count2): (usize, usize),
) {
    loop {
        if count1 == 0 && count2 == 0 {
            return;
        }
        if count1 == 0 {
            mark_changed(y, line2, count2);
            return;
        }
        if count2 == 0 {
            mark_changed(x, line1, count1);
            return;
        }
        let mut lcs = Region::default();
        match find_lcs(x, y, &mut lcs, (line1, count1), (line2, count2)) {
            Found::TooFrequent => {
                fall_back(x, y, ws, (line1, count1), (line2, count2));
                return;
            }
            Found::Run if lcs.begin1 == 0 && lcs.begin2 == 0 => {
                mark_changed(x, line1, count1);
                mark_changed(y, line2, count2);
                return;
            }
            Found::Run => {
                histogram(
                    x,
                    y,
                    ws,
                    (line1, lcs.begin1 - line1),
                    (line2, lcs.begin2 - line2),
                );
                let (end1, end2) = (line1 + count1 - 1, line2 + count2 - 1);
                count1 = end1 - lcs.end1;
                line1 = lcs.end1 + 1;
                count2 = end2 - lcs.end2;
                line2 = lcs.end2 + 1;
            }
        }
    }
}

enum Found {
    Run,
    TooFrequent,
}

/// Finds the best common run into `lcs` (empty when the sides share no line); `TooFrequent` when every shared line is.
fn find_lcs(
    x: &Side,
    y: &Side,
    lcs: &mut Region,
    (line1, count1): (usize, usize),
    (line2, count2): (usize, usize),
) -> Found {
    let end1 = line1 + count1 - 1;
    let end2 = line2 + count2 - 1;
    let class1 = |l: usize| x.class[l - 1];
    let class2 = |l: usize| y.class[l - 1];
    let mut ix = Index {
        first: HashMap::new(),
        count: HashMap::new(),
        next: vec![0; count1],
        shift: line1,
        cnt: MAX_CHAIN_LENGTH + 1,
        has_common: false,
    };
    for ptr in (line1..=end1).rev() {
        let c = class1(ptr);
        if let Some(first) = ix.first.get(&c).copied() {
            ix.next[ptr - ix.shift] = first;
        }
        ix.first.insert(c, ptr);
        *ix.count.entry(c).or_insert(0) += 1;
    }
    let mut b_ptr = line2;
    while b_ptr <= end2 {
        let mut b_next = b_ptr + 1;
        if let Some(&first) = ix.first.get(&class2(b_ptr)) {
            let rec_cnt = ix.count[&class2(b_ptr)];
            ix.has_common = true;
            if rec_cnt <= ix.cnt {
                let mut as_ = first;
                loop {
                    let mut np = ix.next[as_ - ix.shift];
                    let mut bs = b_ptr;
                    let mut ae = as_;
                    let mut be = bs;
                    let mut rc = rec_cnt;
                    while line1 < as_ && line2 < bs && class1(as_ - 1) == class2(bs - 1) {
                        as_ -= 1;
                        bs -= 1;
                        if rc > 1 {
                            rc = rc.min(ix.count[&class1(as_)]);
                        }
                    }
                    while ae < end1 && be < end2 && class1(ae + 1) == class2(be + 1) {
                        ae += 1;
                        be += 1;
                        if rc > 1 {
                            rc = rc.min(ix.count[&class1(ae)]);
                        }
                    }
                    if b_next <= be {
                        b_next = be + 1;
                    }
                    if lcs.end1 - lcs.begin1 < ae - as_ || rc < ix.cnt {
                        *lcs = Region {
                            begin1: as_,
                            begin2: bs,
                            end1: ae,
                            end2: be,
                        };
                        ix.cnt = rc;
                    }
                    if np == 0 {
                        break;
                    }
                    let mut done = false;
                    while np <= ae {
                        np = ix.next[np - ix.shift];
                        if np == 0 {
                            done = true;
                            break;
                        }
                    }
                    if done {
                        break;
                    }
                    as_ = np;
                }
            }
        }
        b_ptr = b_next;
    }
    if ix.has_common && ix.cnt > MAX_CHAIN_LENGTH {
        Found::TooFrequent
    } else {
        Found::Run
    }
}

//! The line diff `git blame` runs (xdiff's Myers with its cost heuristics, hunk sliding and the indent heuristic),
//! ported so the hunks come out where git puts them.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hunk {
    pub a_start: usize,
    pub a_count: usize,
    pub b_start: usize,
    pub b_count: usize,
}

const MAX_COST_MIN: i64 = 256;
const HEUR_MIN_COST: i64 = 256;
const SNAKE_CNT: i64 = 20;
const K_HEUR: i64 = 4;
const MAX_EQLIMIT: i64 = 1024;
const SIMSCAN_WINDOW: i64 = 100;
const KPDIS_RUN: i64 = 4;
const LINE_MAX: i64 = i64::MAX;

const DISCARD: u8 = 0;
const KEEP: u8 = 1;
const INVESTIGATE: u8 = 2;

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// The hunks turning `a` into `b`, as xdiff reports them with no context.
pub fn hunks(a: &[u8], b: &[u8], ignore_whitespace: bool, indent_heuristic: bool) -> Vec<Hunk> {
    let ws = if ignore_whitespace {
        pyn_diff::Ws::all()
    } else {
        pyn_diff::Ws::default()
    };
    hunks_ws(a, b, &ws, indent_heuristic)
}

/// The edit script from `old` to `new` that git's diff would print.
pub fn edits(old: &[u8], new: &[u8], ws: &pyn_diff::Ws) -> Vec<pyn_diff::Edit> {
    hunks_ws(old, new, ws, true)
        .iter()
        .map(|h| (h.a_start, h.a_count, h.b_start, h.b_count))
        .collect()
}

/// The hunks of a unified diff cut as `layout` says, found by this xdiff port.
pub fn unified(
    old: &[u8],
    new: &[u8],
    layout: &pyn_diff::Layout,
    ws: &pyn_diff::Ws,
) -> Vec<pyn_diff::Hunk> {
    pyn_diff::hunks_from_script(old, new, layout, ws, &edits(old, new, ws))
}

/// Lines added and deleted going from `old` to `new`, as the diffstat counts them.
pub fn line_counts(old: &[u8], new: &[u8], ws: &pyn_diff::Ws) -> (usize, usize) {
    if ws.blank_lines {
        let layout = pyn_diff::Layout::default();
        return unified(old, new, &layout, ws)
            .iter()
            .flat_map(|h| h.lines.iter())
            .fold((0, 0), |(a, d), (op, _)| match op {
                pyn_diff::Op::Insert => (a + 1, d),
                pyn_diff::Op::Delete => (a, d + 1),
                pyn_diff::Op::Equal => (a, d),
            });
    }
    hunks_ws(old, new, ws, true)
        .iter()
        .fold((0, 0), |(a, d), h| (a + h.b_count, d + h.a_count))
}

/// `hunks` comparing lines as `ws` says.
pub fn hunks_ws(a: &[u8], b: &[u8], ws: &pyn_diff::Ws, indent_heuristic: bool) -> Vec<Hunk> {
    let (a, b) = trim_common_tail(a, b);
    let (mut x, mut y) = prepare(a, b, ws);
    recs_cmp_all(&mut x, &mut y, ws.minimal);
    let indent_heuristic = indent_heuristic && !ws.no_indent_heuristic;
    compact(&mut x, &mut y, indent_heuristic);
    compact(&mut y, &mut x, indent_heuristic);
    script(&x, &y)
}

/// Drops identical trailing 1 KiB blocks, as `xdi_diff` does, keeping whole lines.
fn trim_common_tail<'a>(a: &'a [u8], b: &'a [u8]) -> (&'a [u8], &'a [u8]) {
    const BLK: usize = 1024;
    let smaller = a.len().min(b.len());
    let mut trimmed = 0;
    while BLK + trimmed <= smaller
        && a[a.len() - trimmed - BLK..a.len() - trimmed]
            == b[b.len() - trimmed - BLK..b.len() - trimmed]
    {
        trimmed += BLK;
    }
    let tail = &a[a.len() - trimmed..];
    let mut recovered = 0;
    while recovered < trimmed {
        recovered += 1;
        if tail[recovered - 1] == b'\n' {
            break;
        }
    }
    let cut = trimmed - recovered;
    (&a[..a.len() - cut], &b[..b.len() - cut])
}

struct Side<'a> {
    recs: Vec<&'a [u8]>,
    class: Vec<usize>,
    changed: Vec<bool>,
    index: Vec<usize>,
    nreff: i64,
    dstart: i64,
    dend: i64,
}

impl Side<'_> {
    fn n(&self) -> i64 {
        self.recs.len() as i64
    }

    fn ch(&self, i: i64) -> bool {
        usize::try_from(i + 1)
            .ok()
            .and_then(|k| self.changed.get(k))
            .copied()
            .unwrap_or(false)
    }

    fn set(&mut self, i: i64, v: bool) {
        self.changed[(i + 1) as usize] = v;
    }

    fn hash(&self, i: i64) -> usize {
        self.class[self.index[i as usize]]
    }
}

fn records(data: &[u8]) -> Vec<&[u8]> {
    data.split_inclusive(|b| *b == b'\n').collect()
}

fn prepare<'a>(a: &'a [u8], b: &'a [u8], ws: &pyn_diff::Ws) -> (Side<'a>, Side<'a>) {
    let mut classes: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut lens: Vec<(i64, i64)> = Vec::new();
    let mut classify = |recs: &[&[u8]], pass: usize| -> Vec<usize> {
        recs.iter()
            .map(|r| {
                let key = ws.key(r);
                let next = classes.len();
                let id = *classes.entry(key).or_insert(next);
                if id == lens.len() {
                    lens.push((0, 0));
                }
                if pass == 1 {
                    lens[id].0 += 1;
                } else {
                    lens[id].1 += 1;
                }
                id
            })
            .collect()
    };
    let (ra, rb) = (records(a), records(b));
    let (ca, cb) = (classify(&ra, 1), classify(&rb, 2));
    let side = |recs: Vec<&'a [u8]>, class: Vec<usize>| Side {
        changed: vec![false; recs.len() + 2],
        recs,
        class,
        index: Vec::new(),
        nreff: 0,
        dstart: 0,
        dend: 0,
    };
    let (mut x, mut y) = (side(ra, ca), side(rb, cb));
    trim_ends(&mut x, &mut y);
    cleanup(&lens, &mut x, &mut y);
    (x, y)
}

fn trim_ends(x: &mut Side, y: &mut Side) {
    let lim = x.recs.len().min(y.recs.len());
    let mut i = 0;
    while i < lim && x.class[i] == y.class[i] {
        i += 1;
    }
    x.dstart = i as i64;
    y.dstart = i as i64;
    let rest = lim - i;
    let mut j = 0;
    while j < rest && x.class[x.recs.len() - 1 - j] == y.class[y.recs.len() - 1 - j] {
        j += 1;
    }
    x.dend = x.n() - j as i64 - 1;
    y.dend = y.n() - j as i64 - 1;
}

fn bogosqrt(mut n: u64) -> i64 {
    let mut i: i64 = 1;
    while n > 0 {
        i <<= 1;
        n >>= 2;
    }
    i
}

fn clean_mmatch(action: &[u8], i: i64, len: i64) -> bool {
    let mut s = 0;
    let mut e = len - 1;
    if i - s > SIMSCAN_WINDOW {
        s = i - SIMSCAN_WINDOW;
    }
    if e - i > SIMSCAN_WINDOW {
        e = i + SIMSCAN_WINDOW;
    }
    let (mut rdis0, mut rpdis0) = (0, 1);
    let mut r = 1;
    while i - r >= s {
        match action[(i - r) as usize] {
            DISCARD => rdis0 += 1,
            INVESTIGATE => rpdis0 += 1,
            _ => break,
        }
        r += 1;
    }
    if rdis0 == 0 {
        return false;
    }
    let (mut rdis1, mut rpdis1) = (0, 1);
    let mut r = 1;
    while i + r <= e {
        match action[(i + r) as usize] {
            DISCARD => rdis1 += 1,
            INVESTIGATE => rpdis1 += 1,
            _ => break,
        }
        r += 1;
    }
    if rdis1 == 0 {
        return false;
    }
    rdis1 += rdis0;
    rpdis1 += rpdis0;
    rpdis1 * KPDIS_RUN < rpdis1 + rdis1
}

fn cleanup(lens: &[(i64, i64)], x: &mut Side, y: &mut Side) {
    let off = x.dstart;
    let len1 = x.dend - off + 1;
    let len2 = y.dend - off + 1;
    let limit = |n: i64| bogosqrt(n as u64).min(MAX_EQLIMIT);
    let (mlim1, mlim2) = (limit(x.n()), limit(y.n()));
    let actions = |side: &Side, len: i64, mlim: i64, other_len: bool| -> Vec<u8> {
        (0..len)
            .map(|i| {
                let c = side.class[(i + off) as usize];
                let nm = if other_len { lens[c].1 } else { lens[c].0 };
                if nm == 0 {
                    DISCARD
                } else if nm < mlim {
                    KEEP
                } else {
                    INVESTIGATE
                }
            })
            .collect()
    };
    let a1 = actions(x, len1, mlim1, true);
    let a2 = actions(y, len2, mlim2, false);
    apply(x, &a1, len1, off);
    apply(y, &a2, len2, off);
}

fn apply(side: &mut Side, actions: &[u8], len: i64, off: i64) {
    side.index.clear();
    for i in 0..len {
        let mut action = actions[i as usize];
        if action == INVESTIGATE {
            action = if clean_mmatch(actions, i, len) {
                DISCARD
            } else {
                KEEP
            };
        }
        if action == KEEP {
            side.index.push((i + off) as usize);
        } else {
            side.set(i + off, true);
        }
    }
    side.nreff = side.index.len() as i64;
}

struct Split {
    i1: i64,
    i2: i64,
    min_lo: bool,
    min_hi: bool,
}

struct Env {
    kv: Vec<i64>,
    fo: i64,
    bo: i64,
    mxcost: i64,
}

impl Env {
    fn f(&self, d: i64) -> i64 {
        self.kv[(self.fo + d) as usize]
    }

    fn setf(&mut self, d: i64, v: i64) {
        let at = (self.fo + d) as usize;
        self.kv[at] = v;
    }

    fn b(&self, d: i64) -> i64 {
        self.kv[(self.bo + d) as usize]
    }

    fn setb(&mut self, d: i64, v: i64) {
        let at = (self.bo + d) as usize;
        self.kv[at] = v;
    }
}

#[allow(clippy::too_many_arguments)]
fn split(
    x: &Side,
    off1: i64,
    lim1: i64,
    y: &Side,
    off2: i64,
    lim2: i64,
    need_min: bool,
    env: &mut Env,
) -> Split {
    let dmin = off1 - lim2;
    let dmax = lim1 - off2;
    let fmid = off1 - off2;
    let bmid = lim1 - lim2;
    let odd = (fmid - bmid) & 1 != 0;
    let (mut fmin, mut fmax) = (fmid, fmid);
    let (mut bmin, mut bmax) = (bmid, bmid);
    env.setf(fmid, off1);
    env.setb(bmid, lim1);
    let mut ec = 1;
    loop {
        let mut got_snake = false;
        if fmin > dmin {
            fmin -= 1;
            env.setf(fmin - 1, -1);
        } else {
            fmin += 1;
        }
        if fmax < dmax {
            fmax += 1;
            env.setf(fmax + 1, -1);
        } else {
            fmax -= 1;
        }
        let mut d = fmax;
        while d >= fmin {
            let mut i1 = if env.f(d - 1) >= env.f(d + 1) {
                env.f(d - 1) + 1
            } else {
                env.f(d + 1)
            };
            let prev1 = i1;
            let mut i2 = i1 - d;
            while i1 < lim1 && i2 < lim2 && x.hash(i1) == y.hash(i2) {
                i1 += 1;
                i2 += 1;
            }
            if i1 - prev1 > SNAKE_CNT {
                got_snake = true;
            }
            env.setf(d, i1);
            if odd && bmin <= d && d <= bmax && env.b(d) <= i1 {
                return Split {
                    i1,
                    i2,
                    min_lo: true,
                    min_hi: true,
                };
            }
            d -= 2;
        }
        if bmin > dmin {
            bmin -= 1;
            env.setb(bmin - 1, LINE_MAX);
        } else {
            bmin += 1;
        }
        if bmax < dmax {
            bmax += 1;
            env.setb(bmax + 1, LINE_MAX);
        } else {
            bmax -= 1;
        }
        let mut d = bmax;
        while d >= bmin {
            let mut i1 = if env.b(d - 1) < env.b(d + 1) {
                env.b(d - 1)
            } else {
                env.b(d + 1) - 1
            };
            let prev1 = i1;
            let mut i2 = i1 - d;
            while i1 > off1 && i2 > off2 && x.hash(i1 - 1) == y.hash(i2 - 1) {
                i1 -= 1;
                i2 -= 1;
            }
            if prev1 - i1 > SNAKE_CNT {
                got_snake = true;
            }
            env.setb(d, i1);
            if !odd && fmin <= d && d <= fmax && i1 <= env.f(d) {
                return Split {
                    i1,
                    i2,
                    min_lo: true,
                    min_hi: true,
                };
            }
            d -= 2;
        }
        if need_min {
            ec += 1;
            continue;
        }
        if got_snake && ec > HEUR_MIN_COST {
            let mut best = 0;
            let mut found = (0, 0);
            let mut d = fmax;
            while d >= fmin {
                let dd = (d - fmid).abs();
                let i1 = env.f(d);
                let i2 = i1 - d;
                let v = (i1 - off1) + (i2 - off2) - dd;
                if v > K_HEUR * ec
                    && v > best
                    && off1 + SNAKE_CNT <= i1
                    && i1 < lim1
                    && off2 + SNAKE_CNT <= i2
                    && i2 < lim2
                {
                    let mut k = 1;
                    while x.hash(i1 - k) == y.hash(i2 - k) {
                        if k == SNAKE_CNT {
                            best = v;
                            found = (i1, i2);
                            break;
                        }
                        k += 1;
                    }
                }
                d -= 2;
            }
            if best > 0 {
                return Split {
                    i1: found.0,
                    i2: found.1,
                    min_lo: true,
                    min_hi: false,
                };
            }
            let mut best = 0;
            let mut d = bmax;
            while d >= bmin {
                let dd = (d - bmid).abs();
                let i1 = env.b(d);
                let i2 = i1 - d;
                let v = (lim1 - i1) + (lim2 - i2) - dd;
                if v > K_HEUR * ec
                    && v > best
                    && off1 < i1
                    && i1 <= lim1 - SNAKE_CNT
                    && off2 < i2
                    && i2 <= lim2 - SNAKE_CNT
                {
                    let mut k = 0;
                    while x.hash(i1 + k) == y.hash(i2 + k) {
                        if k == SNAKE_CNT - 1 {
                            best = v;
                            found = (i1, i2);
                            break;
                        }
                        k += 1;
                    }
                }
                d -= 2;
            }
            if best > 0 {
                return Split {
                    i1: found.0,
                    i2: found.1,
                    min_lo: false,
                    min_hi: true,
                };
            }
        }
        if ec >= env.mxcost {
            let (mut fbest, mut fbest1) = (-1, -1);
            let mut d = fmax;
            while d >= fmin {
                let mut i1 = env.f(d).min(lim1);
                let mut i2 = i1 - d;
                if lim2 < i2 {
                    i1 = lim2 + d;
                    i2 = lim2;
                }
                if fbest < i1 + i2 {
                    fbest = i1 + i2;
                    fbest1 = i1;
                }
                d -= 2;
            }
            let (mut bbest, mut bbest1) = (LINE_MAX, LINE_MAX);
            let mut d = bmax;
            while d >= bmin {
                let mut i1 = off1.max(env.b(d));
                let mut i2 = i1 - d;
                if i2 < off2 {
                    i1 = off2 + d;
                    i2 = off2;
                }
                if i1 + i2 < bbest {
                    bbest = i1 + i2;
                    bbest1 = i1;
                }
                d -= 2;
            }
            return if (lim1 + lim2) - bbest < fbest - (off1 + off2) {
                Split {
                    i1: fbest1,
                    i2: fbest - fbest1,
                    min_lo: true,
                    min_hi: false,
                }
            } else {
                Split {
                    i1: bbest1,
                    i2: bbest - bbest1,
                    min_lo: false,
                    min_hi: true,
                }
            };
        }
        ec += 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn recs_cmp(
    x: &mut Side,
    mut off1: i64,
    mut lim1: i64,
    y: &mut Side,
    mut off2: i64,
    mut lim2: i64,
    need_min: bool,
    env: &mut Env,
) {
    while off1 < lim1 && off2 < lim2 && x.hash(off1) == y.hash(off2) {
        off1 += 1;
        off2 += 1;
    }
    while off1 < lim1 && off2 < lim2 && x.hash(lim1 - 1) == y.hash(lim2 - 1) {
        lim1 -= 1;
        lim2 -= 1;
    }
    if off1 == lim1 {
        while off2 < lim2 {
            let at = y.index[off2 as usize] as i64;
            y.set(at, true);
            off2 += 1;
        }
    } else if off2 == lim2 {
        while off1 < lim1 {
            let at = x.index[off1 as usize] as i64;
            x.set(at, true);
            off1 += 1;
        }
    } else {
        let spl = split(x, off1, lim1, y, off2, lim2, need_min, env);
        recs_cmp(x, off1, spl.i1, y, off2, spl.i2, spl.min_lo, env);
        recs_cmp(x, spl.i1, lim1, y, spl.i2, lim2, spl.min_hi, env);
    }
}

fn recs_cmp_all(x: &mut Side, y: &mut Side, minimal: bool) {
    let ndiags = x.nreff + y.nreff + 3;
    let mut env = Env {
        kv: vec![0; (2 * ndiags + 2) as usize],
        fo: y.nreff + 1,
        bo: ndiags + y.nreff + 1,
        mxcost: bogosqrt(ndiags as u64).max(MAX_COST_MIN),
    };
    let (n1, n2) = (x.nreff, y.nreff);
    recs_cmp(x, 0, n1, y, 0, n2, minimal, &mut env);
}

fn get_indent(rec: &[u8]) -> i32 {
    let mut ret = 0;
    for &c in rec {
        if !is_space(c) {
            return ret;
        } else if c == b' ' {
            ret += 1;
        } else if c == b'\t' {
            ret += 8 - ret % 8;
        }
        if ret >= 200 {
            return 200;
        }
    }
    -1
}

struct Measure {
    end_of_file: bool,
    indent: i32,
    pre_blank: i32,
    pre_indent: i32,
    post_blank: i32,
    post_indent: i32,
}

fn measure_split(side: &Side, at: i64) -> Measure {
    let n = side.n();
    let (end_of_file, indent) = if at >= n {
        (true, -1)
    } else {
        (false, get_indent(side.recs[at as usize]))
    };
    let (mut pre_blank, mut pre_indent) = (0, -1);
    let mut i = at - 1;
    while i >= 0 {
        pre_indent = get_indent(side.recs[i as usize]);
        if pre_indent != -1 {
            break;
        }
        pre_blank += 1;
        if pre_blank == 20 {
            pre_indent = 0;
            break;
        }
        i -= 1;
    }
    let (mut post_blank, mut post_indent) = (0, -1);
    let mut i = at + 1;
    while i < n {
        post_indent = get_indent(side.recs[i as usize]);
        if post_indent != -1 {
            break;
        }
        post_blank += 1;
        if post_blank == 20 {
            post_indent = 0;
            break;
        }
        i += 1;
    }
    Measure {
        end_of_file,
        indent,
        pre_blank,
        pre_indent,
        post_blank,
        post_indent,
    }
}

#[derive(Clone, Copy, Default)]
struct Score {
    effective_indent: i32,
    penalty: i32,
}

fn score_add_split(m: &Measure, s: &mut Score) {
    if m.pre_indent == -1 && m.pre_blank == 0 {
        s.penalty += 1;
    }
    if m.end_of_file {
        s.penalty += 21;
    }
    let post_blank = if m.indent == -1 { 1 + m.post_blank } else { 0 };
    let total_blank = m.pre_blank + post_blank;
    s.penalty += -30 * total_blank;
    s.penalty += 6 * post_blank;
    let indent = if m.indent != -1 {
        m.indent
    } else {
        m.post_indent
    };
    let any_blanks = total_blank != 0;
    s.effective_indent += indent;
    if indent == -1 || m.pre_indent == -1 {
    } else if indent > m.pre_indent {
        s.penalty += if any_blanks { 10 } else { -4 };
    } else if indent == m.pre_indent {
    } else if m.post_indent != -1 && m.post_indent > indent {
        s.penalty += if any_blanks { 17 } else { 24 };
    } else {
        s.penalty += if any_blanks { 17 } else { 23 };
    }
}

fn score_cmp(a: &Score, b: &Score) -> i32 {
    let cmp = (a.effective_indent > b.effective_indent) as i32
        - (a.effective_indent < b.effective_indent) as i32;
    60 * cmp + (a.penalty - b.penalty)
}

#[derive(Clone, Copy)]
struct Group {
    start: i64,
    end: i64,
}

fn group_init(side: &Side) -> Group {
    let mut g = Group { start: 0, end: 0 };
    while side.ch(g.end) {
        g.end += 1;
    }
    g
}

fn group_next(side: &Side, g: &mut Group) -> bool {
    if g.end == side.n() {
        return false;
    }
    g.start = g.end + 1;
    g.end = g.start;
    while side.ch(g.end) {
        g.end += 1;
    }
    true
}

fn group_previous(side: &Side, g: &mut Group) -> bool {
    if g.start == 0 {
        return false;
    }
    g.end = g.start - 1;
    g.start = g.end;
    while side.ch(g.start - 1) {
        g.start -= 1;
    }
    true
}

fn slide_down(side: &mut Side, g: &mut Group) -> bool {
    if g.end < side.n() && side.class[g.start as usize] == side.class[g.end as usize] {
        side.set(g.start, false);
        g.start += 1;
        side.set(g.end, true);
        g.end += 1;
        while side.ch(g.end) {
            g.end += 1;
        }
        true
    } else {
        false
    }
}

fn slide_up(side: &mut Side, g: &mut Group) -> bool {
    if g.start > 0 && side.class[(g.start - 1) as usize] == side.class[(g.end - 1) as usize] {
        g.start -= 1;
        side.set(g.start, true);
        g.end -= 1;
        side.set(g.end, false);
        while side.ch(g.start - 1) {
            g.start -= 1;
        }
        true
    } else {
        false
    }
}

/// `xdl_change_compact`: slides each changed group to where it merges with the other file's changes or reads best.
fn compact(x: &mut Side, o: &mut Side, indent_heuristic: bool) {
    let mut g = group_init(x);
    let mut go = group_init(o);
    loop {
        if g.end != g.start {
            let mut earliest_end;
            let mut end_matching_other;
            let mut groupsize;
            loop {
                groupsize = g.end - g.start;
                end_matching_other = -1;
                while slide_up(x, &mut g) {
                    group_previous(o, &mut go);
                }
                earliest_end = g.end;
                if go.end > go.start {
                    end_matching_other = g.end;
                }
                loop {
                    if !slide_down(x, &mut g) {
                        break;
                    }
                    group_next(o, &mut go);
                    if go.end > go.start {
                        end_matching_other = g.end;
                    }
                }
                if groupsize == g.end - g.start {
                    break;
                }
            }
            if g.end == earliest_end {
            } else if end_matching_other != -1 {
                while go.end == go.start {
                    slide_up(x, &mut g);
                    group_previous(o, &mut go);
                }
            } else if indent_heuristic {
                let mut shift = earliest_end;
                if g.end - groupsize - 1 > shift {
                    shift = g.end - groupsize - 1;
                }
                if g.end - 100 > shift {
                    shift = g.end - 100;
                }
                let mut best_shift = -1;
                let mut best = Score::default();
                while shift <= g.end {
                    let mut score = Score::default();
                    score_add_split(&measure_split(x, shift), &mut score);
                    score_add_split(&measure_split(x, shift - groupsize), &mut score);
                    if best_shift == -1 || score_cmp(&score, &best) <= 0 {
                        best = score;
                        best_shift = shift;
                    }
                    shift += 1;
                }
                while g.end > best_shift {
                    slide_up(x, &mut g);
                    group_previous(o, &mut go);
                }
            }
        }
        if !group_next(x, &mut g) {
            break;
        }
        group_next(o, &mut go);
    }
}

fn script(x: &Side, y: &Side) -> Vec<Hunk> {
    let mut out = Vec::new();
    let (mut i1, mut i2) = (x.n(), y.n());
    while i1 >= 0 || i2 >= 0 {
        if x.ch(i1 - 1) || y.ch(i2 - 1) {
            let (l1, l2) = (i1, i2);
            while x.ch(i1 - 1) {
                i1 -= 1;
            }
            while y.ch(i2 - 1) {
                i2 -= 1;
            }
            out.push(Hunk {
                a_start: i1 as usize,
                a_count: (l1 - i1) as usize,
                b_start: i2 as usize,
                b_count: (l2 - i2) as usize,
            });
        }
        i1 -= 1;
        i2 -= 1;
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(a_start: usize, a_count: usize, b_start: usize, b_count: usize) -> Hunk {
        Hunk {
            a_start,
            a_count,
            b_start,
            b_count,
        }
    }

    #[test]
    fn equal_files_have_no_hunks_and_empty_sides_are_one_hunk() {
        assert!(hunks(b"a\nb\n", b"a\nb\n", false, true).is_empty());
        assert_eq!(hunks(b"", b"a\nb\n", false, true), vec![h(0, 0, 0, 2)]);
        assert_eq!(hunks(b"a\nb\n", b"", false, true), vec![h(0, 2, 0, 0)]);
    }

    #[test]
    fn a_changed_line_is_one_hunk_and_a_missing_final_newline_is_a_change() {
        assert_eq!(
            hunks(b"a\nb\nc\n", b"a\nX\nc\n", false, true),
            vec![h(1, 1, 1, 1)]
        );
        assert_eq!(hunks(b"a\nb\n", b"a\nb", false, true), vec![h(1, 1, 1, 1)]);
    }

    #[test]
    fn whitespace_is_ignored_only_on_request() {
        assert_eq!(hunks(b"a b\n", b"a  b\n", false, true), vec![h(0, 1, 0, 1)]);
        assert!(hunks(b"a b\n", b"a  b\n", true, true).is_empty());
    }

    #[test]
    fn an_inserted_block_slides_to_where_the_indent_heuristic_wants_it() {
        let a = b"fn a() {\n    x();\n}\n\nfn c() {\n}\n";
        let b = b"fn a() {\n    x();\n}\n\nfn b() {\n    y();\n}\n\nfn c() {\n}\n";
        assert_eq!(hunks(a, b, false, true), vec![h(4, 0, 4, 4)]);
    }
}

//! Combined diffs of a merge (`-c`, `--cc`): the result against every parent at once, laid out as git's
//! `combine-diff.c` does so the hunks, columns and omissions come out the same.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::Result;
use pyn_gitwork::Pathspec;

use super::GitHome;
use super::commands::{add_changes, add_unchanged};
use crate::backend::Backend;
use crate::diff::{self, Blob, Tree};
use crate::rename;

/// One path every parent differs from the result at: what each parent had (after renames) and the result's blob.
pub(super) struct Element {
    pub path: String,
    pub result: Option<Blob>,
    pub parents: Vec<(Option<Blob>, char)>,
}

/// The paths that differ from every parent, in path order.
pub(super) fn elements(
    git: &GitHome,
    result_tree: &pyn_object::ObjectId,
    parent_trees: &[pyn_object::ObjectId],
    (spec, only): (&Pathspec, &[String]),
    renames: rename::Request,
) -> Result<Vec<Element>> {
    let mut out: Vec<Element> = Vec::new();
    for (n, parent) in parent_trees.iter().enumerate() {
        let (mut old, mut new) = (Tree::new(), Tree::new());
        let changes = git.repo.diff_trees(Some(parent), Some(result_tree), spec)?;
        add_changes(git, &mut old, &mut new, &changes)?;
        if renames.is_harder(git.rename_defaults()) {
            add_unchanged(
                git,
                (&mut old, &mut new),
                (Some(parent), Some(result_tree)),
                spec,
            )?;
        }
        let mut found: BTreeMap<String, diff::Origin> =
            diff::origins(git, &old, &new, only, renames)?
                .into_iter()
                .map(|o| (o.path.clone(), o))
                .collect();
        if n == 0 {
            out = found
                .into_values()
                .map(|o| Element {
                    path: o.path,
                    result: o.to,
                    parents: vec![(o.from, o.status)],
                })
                .collect();
        } else {
            out.retain_mut(|e| match found.remove(&e.path) {
                Some(o) => {
                    e.parents.push((o.from, o.status));
                    true
                }
                None => false,
            });
        }
    }
    Ok(out)
}

/// The `--name-only` line of a path, or its `--name-status` line with a status letter per parent.
pub(super) fn name_line(git: &GitHome, e: &Element, status: bool) -> String {
    let path = git.quote_diff_path(e.path.as_bytes());
    if status {
        let letters: String = e.parents.iter().map(|(_, s)| *s).collect();
        format!("{letters}\t{path}\n")
    } else {
        format!("{path}\n")
    }
}

struct LLine {
    text: Vec<u8>,
    map: u64,
}

#[derive(Default)]
struct SLine {
    lost: Vec<LLine>,
    plost: Vec<Vec<u8>>,
    flag: u64,
    p_lno: Vec<usize>,
}

fn lines(data: &[u8]) -> Vec<&[u8]> {
    data.split_inclusive(|b| *b == b'\n').collect()
}

fn chomp(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

/// Merges the parent's lost lines into those gathered from earlier parents by their longest common subsequence.
fn coalesce(base: &mut Vec<LLine>, new: Vec<Vec<u8>>, parent: usize) {
    if new.is_empty() {
        return;
    }
    let bit = 1u64 << parent;
    if base.is_empty() {
        *base = new
            .into_iter()
            .map(|text| LLine { text, map: bit })
            .collect();
        return;
    }
    let (nb, nn) = (base.len(), new.len());
    let mut lcs = vec![vec![0usize; nn + 1]; nb + 1];
    // 0 = match, 1 = base only, 2 = new only
    let mut dir = vec![vec![0u8; nn + 1]; nb + 1];
    for row in dir.iter_mut().skip(1) {
        row[0] = 1;
    }
    for d in dir[0].iter_mut().skip(1) {
        *d = 2;
    }
    for i in 1..=nb {
        for j in 1..=nn {
            if base[i - 1].text == new[j - 1] {
                lcs[i][j] = lcs[i - 1][j - 1] + 1;
                dir[i][j] = 0;
            } else if lcs[i][j - 1] >= lcs[i - 1][j] {
                lcs[i][j] = lcs[i][j - 1];
                dir[i][j] = 2;
            } else {
                lcs[i][j] = lcs[i - 1][j];
                dir[i][j] = 1;
            }
        }
    }
    let mut old: Vec<Option<LLine>> = std::mem::take(base).into_iter().map(Some).collect();
    let mut news: Vec<Option<Vec<u8>>> = new.into_iter().map(Some).collect();
    let (mut i, mut j) = (nb, nn);
    let mut rev: Vec<LLine> = Vec::new();
    while i != 0 || j != 0 {
        match dir[i][j] {
            0 => {
                let mut l = old[i - 1].take().expect("a base line is used once");
                l.map |= bit;
                rev.push(l);
                i -= 1;
                j -= 1;
            }
            2 => {
                let text = news[j - 1].take().expect("a new line is used once");
                rev.push(LLine { text, map: bit });
                j -= 1;
            }
            _ => {
                rev.push(old[i - 1].take().expect("a base line is used once"));
                i -= 1;
            }
        }
    }
    rev.reverse();
    *base = rev;
}

/// Marks the result lines the parent lacks and hangs the lines it lost on the line that follows them.
fn combine_one(parent: &[u8], result: &[u8], sline: &mut [SLine], n: usize, cnt: usize) {
    let nmask = 1u64 << n;
    let plines = lines(parent);
    for h in crate::xdiff::hunks(parent, result, false, true) {
        for l in &plines[h.a_start..h.a_start + h.a_count] {
            sline[h.b_start].plost.push(chomp(l).to_vec());
        }
        for s in &mut sline[h.b_start..h.b_start + h.b_count] {
            s.flag |= nmask;
        }
    }
    let mut p_lno = 1;
    for (lno, line) in sline.iter_mut().enumerate().take(cnt + 1) {
        line.p_lno[n] = p_lno;
        let plost = std::mem::take(&mut line.plost);
        coalesce(&mut line.lost, plost, n);
        p_lno += line.lost.iter().filter(|l| l.map & nmask != 0).count();
        if lno < cnt && line.flag & nmask == 0 {
            p_lno += 1;
        }
    }
    sline[cnt + 1].p_lno[n] = p_lno;
}

/// Parent `i` has the same content as the already examined parent `j`: copy its result.
fn reuse(sline: &mut [SLine], cnt: usize, i: usize, j: usize) {
    let (imask, jmask) = (1u64 << i, 1u64 << j);
    for s in sline.iter_mut().take(cnt + 1) {
        s.p_lno[i] = s.p_lno[j];
        for l in &mut s.lost {
            if l.map & jmask != 0 {
                l.map |= imask;
            }
        }
        if s.flag & jmask != 0 {
            s.flag |= imask;
        }
    }
    sline[cnt + 1].p_lno[i] = sline[cnt + 1].p_lno[j];
}

fn find_next(s: &[SLine], mark: u64, mut i: usize, cnt: usize, uninteresting: bool) -> usize {
    while i <= cnt {
        if (s[i].flag & mark == 0) == uninteresting {
            return i;
        }
        i += 1;
    }
    i
}

fn adjust_hunk_tail(s: &[SLine], all_mask: u64, hunk_begin: usize, i: usize) -> usize {
    if hunk_begin < i && s[i - 1].flag & all_mask == 0 {
        i - 1
    } else {
        i
    }
}

fn give_context(s: &mut [SLine], cnt: usize, np: usize, context: usize) -> bool {
    let all_mask = (1u64 << np) - 1;
    let (mark, no_pre_delete) = (1u64 << np, 2u64 << np);
    let mut i = find_next(s, mark, 0, cnt, false);
    if cnt < i {
        return false;
    }
    'outer: while i <= cnt {
        let mut j = i.saturating_sub(context);
        while j < i {
            if s[j].flag & mark == 0 {
                s[j].flag |= no_pre_delete;
            }
            s[j].flag |= mark;
            j += 1;
        }
        loop {
            j = find_next(s, mark, i, cnt, true);
            if cnt < j {
                break 'outer;
            }
            let mut k = find_next(s, mark, j, cnt, false);
            j = adjust_hunk_tail(s, all_mask, i, j);
            if k < j + context {
                while j < k {
                    s[j].flag |= mark;
                    j += 1;
                }
                i = k;
                continue;
            }
            i = k;
            k = if j + context < cnt + 1 {
                j + context
            } else {
                cnt + 1
            };
            while j < k {
                s[j].flag |= mark;
                j += 1;
            }
            break;
        }
    }
    true
}

fn make_hunks(s: &mut [SLine], cnt: usize, np: usize, (dense, context): (bool, usize)) -> bool {
    let all_mask = (1u64 << np) - 1;
    let mark = 1u64 << np;
    for l in s.iter_mut().take(cnt + 1) {
        if l.flag & all_mask != 0 || !l.lost.is_empty() {
            l.flag |= mark;
        } else {
            l.flag &= !mark;
        }
    }
    if !dense {
        return give_context(s, cnt, np, context);
    }
    let mut i = 0;
    while i <= cnt {
        while i <= cnt && s[i].flag & mark == 0 {
            i += 1;
        }
        if cnt < i {
            break;
        }
        let hunk_begin = i;
        let mut j = i + 1;
        while j <= cnt {
            if s[j].flag & mark == 0 {
                let mut contin = false;
                let mut la = adjust_hunk_tail(s, all_mask, hunk_begin, j);
                la = if la + context < cnt + 1 {
                    la + context
                } else {
                    cnt + 1
                };
                while la != 0 {
                    la -= 1;
                    if j > la {
                        break;
                    }
                    if s[la].flag & mark != 0 {
                        contin = true;
                        break;
                    }
                }
                if !contin {
                    break;
                }
                j = la;
            }
            j += 1;
        }
        let hunk_end = j;
        let mut same_diff = 0u64;
        let mut has_interesting = false;
        'scan: for l in &s[i..hunk_end] {
            let this_diff = l.flag & all_mask;
            if this_diff != 0 {
                if same_diff == 0 {
                    same_diff = this_diff;
                } else if same_diff != this_diff {
                    has_interesting = true;
                    break 'scan;
                }
            }
            for ll in &l.lost {
                if same_diff == 0 {
                    same_diff = ll.map;
                } else if same_diff != ll.map {
                    has_interesting = true;
                    break 'scan;
                }
            }
        }
        if !has_interesting && same_diff != all_mask {
            for l in &mut s[hunk_begin..hunk_end] {
                l.flag &= !mark;
            }
        }
        i = hunk_end;
    }
    give_context(s, cnt, np, context)
}

fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn comment_line(line: &[u8]) -> bool {
    line.first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b'$')
}

fn dump(out: &mut Vec<u8>, s: &[SLine], text: &[&[u8]], np: usize, context: usize) {
    let cnt = text.len();
    let mark = 1u64 << np;
    let no_pre_delete = 2u64 << np;
    let mut lno = 0;
    loop {
        let mut comment: Option<&[u8]> = None;
        while lno <= cnt && s[lno].flag & mark == 0 {
            if lno < cnt && comment_line(chomp(text[lno])) {
                comment = Some(chomp(text[lno]));
            }
            lno += 1;
        }
        if cnt < lno {
            break;
        }
        let mut hunk_end = lno + 1;
        while hunk_end <= cnt && s[hunk_end].flag & mark != 0 {
            hunk_end += 1;
        }
        // rlines is unsigned in git and wraps for a deleted file at -U0
        let mut rlines = (hunk_end - lno) as u64;
        if cnt < hunk_end {
            rlines -= 1;
        }
        let mut null_context = 0;
        if context == 0 {
            null_context = s[lno..hunk_end]
                .iter()
                .filter(|l| l.flag & (mark - 1) == 0)
                .count();
            rlines = rlines.wrapping_sub(null_context as u64);
        }
        out.extend(std::iter::repeat_n(b'@', np + 1));
        for n in 0..np {
            let (l0, l1) = (s[lno].p_lno[n], s[hunk_end].p_lno[n]);
            let count = (l1 as u64)
                .wrapping_sub(l0 as u64)
                .wrapping_sub(null_context as u64);
            out.extend_from_slice(format!(" -{l0},{count}").as_bytes());
        }
        out.extend_from_slice(format!(" +{},{} ", lno + 1, rlines).as_bytes());
        out.extend(std::iter::repeat_n(b'@', np + 1));
        if let Some(c) = comment {
            let mut end = 0;
            for (i, ch) in c.iter().take(40).enumerate() {
                if !is_c_space(*ch) {
                    end = i;
                }
            }
            if end != 0 {
                out.push(b' ');
            }
            out.extend_from_slice(&c[..end]);
        }
        out.push(b'\n');
        while lno < hunk_end {
            let sl = &s[lno];
            lno += 1;
            if sl.flag & no_pre_delete == 0 {
                for ll in &sl.lost {
                    for j in 0..np {
                        out.push(if ll.map & (1 << j) != 0 { b'-' } else { b' ' });
                    }
                    out.extend_from_slice(&ll.text);
                    out.push(b'\n');
                }
            }
            if cnt < lno {
                break;
            }
            if sl.flag & (mark - 1) == 0 && context == 0 {
                continue;
            }
            for j in 0..np {
                out.push(if sl.flag & (1 << j) != 0 { b'+' } else { b' ' });
            }
            out.extend_from_slice(chomp(text[lno - 1]));
            out.push(b'\n');
        }
    }
}

fn mode_of(b: Option<&Blob>) -> &str {
    b.map_or("000000", |b| b.mode())
}

fn abbrev(b: Option<&Blob>) -> String {
    b.map_or("0000000".to_string(), |b| b.hash[..7].to_string())
}

fn header(git: &GitHome, e: &Element, dense: bool, file_header: bool) -> String {
    let mut out = String::new();
    let word = if dense {
        "diff --cc "
    } else {
        "diff --combined "
    };
    let _ = writeln!(out, "{word}{}", git.quote_diff_path(e.path.as_bytes()));
    let idx: Vec<String> = e.parents.iter().map(|(b, _)| abbrev(b.as_ref())).collect();
    let _ = writeln!(
        out,
        "index {}..{}",
        idx.join(","),
        abbrev(e.result.as_ref())
    );
    let result_mode = mode_of(e.result.as_ref());
    let mode_differs = e
        .parents
        .iter()
        .any(|(b, _)| mode_of(b.as_ref()) != result_mode);
    let deleted = e.result.is_none();
    let mut added = false;
    if mode_differs {
        added = !deleted && e.parents.iter().all(|(_, s)| *s == 'A');
        if added {
            let _ = writeln!(out, "new file mode {result_mode}");
        } else {
            if deleted {
                out.push_str("deleted file ");
            }
            let modes: Vec<&str> = e.parents.iter().map(|(b, _)| mode_of(b.as_ref())).collect();
            let _ = write!(out, "mode {}", modes.join(","));
            if !deleted {
                let _ = write!(out, "..{result_mode}");
            }
            out.push('\n');
        }
    }
    if file_header {
        if added {
            out.push_str("--- /dev/null\n");
        } else {
            let _ = writeln!(
                out,
                "--- {}",
                git.quote_diff_path(format!("a/{}", e.path).as_bytes())
            );
        }
        if deleted {
            out.push_str("+++ /dev/null\n");
        } else {
            let _ = writeln!(
                out,
                "+++ {}",
                git.quote_diff_path(format!("b/{}", e.path).as_bytes())
            );
        }
    }
    out
}

/// The combined patch of one element; `None` when `--cc` leaves nothing of it to show.
pub(super) fn patch(
    git: &GitHome,
    e: &Element,
    (dense, context): (bool, usize),
) -> Result<Option<String>> {
    let np = e.parents.len();
    let bytes = |b: Option<&Blob>| -> Result<Vec<u8>> {
        b.map_or(Ok(Vec::new()), |b| diff::blob_bytes(git, &e.path, b))
    };
    let result = bytes(e.result.as_ref())?;
    let mut parents = Vec::with_capacity(np);
    for (b, _) in &e.parents {
        parents.push(bytes(b.as_ref())?);
    }
    let result_mode = mode_of(e.result.as_ref());
    let mode_differs = e
        .parents
        .iter()
        .any(|(b, _)| mode_of(b.as_ref()) != result_mode);
    if pyn_diff::is_binary(&result) || parents.iter().any(|p| pyn_diff::is_binary(p)) {
        return Ok(Some(format!(
            "{}Binary files differ\n",
            header(git, e, dense, false)
        )));
    }
    let text = lines(&result);
    let cnt = text.len();
    let mut sline: Vec<SLine> = (0..cnt + 2)
        .map(|_| SLine {
            p_lno: vec![0; np],
            ..SLine::default()
        })
        .collect();
    for (i, parent) in parents.iter().enumerate() {
        let same = (0..i).find(|&j| {
            e.parents[i].0.as_ref().map(|b| &b.hash) == e.parents[j].0.as_ref().map(|b| &b.hash)
        });
        match same {
            Some(j) => reuse(&mut sline, cnt, i, j),
            None => combine_one(parent, &result, &mut sline, i, cnt),
        }
    }
    let show_hunks = make_hunks(&mut sline, cnt, np, (dense, context));
    if !show_hunks && !mode_differs {
        return Ok(None);
    }
    let mut out = header(git, e, dense, true).into_bytes();
    dump(&mut out, &sline, &text, np, context);
    Ok(Some(String::from_utf8_lossy(&out).into_owned()))
}

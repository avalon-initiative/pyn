//! The rename `git blame` follows into a parent: git's exact, basename and inexact matching for one added path.

use std::collections::HashMap;

use anyhow::Result;
use pyn_gitwork::Entry;
use pyn_object::{FileMode, ObjectId};

use super::GitHome;

const MAX_SCORE: u64 = 60_000;
const MIN_SCORE: u64 = 30_000;
const MIN_BASENAME_SCORE: u64 = MIN_SCORE + (MAX_SCORE - MIN_SCORE) / 2;
const HASH_BASE: u32 = 107_927;

fn is_regular(mode: FileMode) -> bool {
    matches!(mode, FileMode::Regular | FileMode::Executable)
}

fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|b| *b == b'/').next().unwrap_or(path)
}

/// git's `basename_same`: the last path components match, or both paths are bare names.
fn basename_same(src: &[u8], dst: &[u8]) -> bool {
    basename(src) == basename(dst)
}

/// Byte counts per span hash as `diffcore-delta.c` keeps them: spans of up to 64 bytes ending at a newline.
fn span_counts(data: &[u8]) -> HashMap<u32, u32> {
    let text = !pyn_diff::is_binary(data);
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let (mut a1, mut a2, mut n) = (0u32, 0u32, 0u32);
    for (i, &c) in data.iter().enumerate() {
        if text && c == b'\r' && data.get(i + 1) == Some(&b'\n') {
            continue;
        }
        let old = a1;
        a1 = (a1 << 7) ^ (a2 >> 25);
        a2 = (a2 << 7) ^ (old >> 25);
        a1 = a1.wrapping_add(u32::from(c));
        n += 1;
        if n < 64 && c != b'\n' {
            continue;
        }
        *counts
            .entry(a1.wrapping_add(a2.wrapping_mul(0x61)) % HASH_BASE)
            .or_insert(0) += n;
        n = 0;
        a1 = 0;
        a2 = 0;
    }
    if n > 0 {
        *counts
            .entry(a1.wrapping_add(a2.wrapping_mul(0x61)) % HASH_BASE)
            .or_insert(0) += n;
    }
    counts
}

/// `estimate_similarity`: the share of the larger file taken from the source, out of `MAX_SCORE`, or 0 if too different.
fn estimate(src: &[u8], dst: &[u8], minimum: u64) -> u64 {
    let (max_size, base_size) = (
        src.len().max(dst.len()) as u64,
        src.len().min(dst.len()) as u64,
    );
    let delta = max_size - base_size;
    if max_size * (MAX_SCORE - minimum) < delta * MAX_SCORE || dst.is_empty() {
        return 0;
    }
    let (s, d) = (span_counts(src), span_counts(dst));
    let copied: u64 = s
        .iter()
        .map(|(h, n)| u64::from(*n.min(d.get(h).unwrap_or(&0))))
        .sum();
    copied * MAX_SCORE / max_size
}

#[derive(Clone, Copy)]
struct Slot {
    src: usize,
    score: i64,
    name_score: i64,
    used: bool,
}

/// `score_compare` for one destination: positive when `a` ranks after `b`.
fn compare(a: &Slot, b: &Slot) -> i64 {
    if !a.used {
        return i64::from(b.used);
    }
    if !b.used {
        return -1;
    }
    if a.score == b.score {
        return b.name_score - a.name_score;
    }
    b.score - a.score
}

/// The index in `sources` (deleted files, in path order) that `dst` was renamed from, as `diffcore_rename` decides.
pub(super) fn find_source(
    git: &GitHome,
    sources: &[(Vec<u8>, Entry)],
    dst_path: &[u8],
    dst: Entry,
    dst_data: &[u8],
) -> Result<Option<usize>> {
    let mut best: Option<(usize, i32)> = None;
    let mut left = 100;
    for (i, (path, e)) in sources.iter().enumerate() {
        if e.id != dst.id || (!(is_regular(e.mode) && is_regular(dst.mode)) && e.mode != dst.mode) {
            continue;
        }
        let score = 1 + i32::from(basename_same(path, dst_path));
        if best.is_none_or(|(_, b)| score > b) {
            best = Some((i, score));
            if score == 2 {
                break;
            }
        }
        left -= 1;
        if left == 0 {
            break;
        }
    }
    if let Some((i, _)) = best {
        return Ok(Some(i));
    }
    if !is_regular(dst.mode) {
        return Ok(None);
    }
    let base = basename(dst_path);
    let same: Vec<usize> = (0..sources.len())
        .filter(|i| basename(&sources[*i].0) == base)
        .collect();
    if let [only] = same[..]
        && is_regular(sources[only].1.mode)
    {
        let data = git.repo.blob(&sources[only].1.id)?;
        if estimate(&data, dst_data, MIN_BASENAME_SCORE) >= MIN_BASENAME_SCORE {
            return Ok(Some(only));
        }
    }
    let mut slots = [Slot {
        src: 0,
        score: 0,
        name_score: 0,
        used: false,
    }; 4];
    for (j, (path, e)) in sources.iter().enumerate() {
        let score = if is_regular(e.mode) {
            estimate(&git.repo.blob(&e.id)?, dst_data, MIN_SCORE)
        } else {
            0
        };
        let this = Slot {
            src: j,
            score: score as i64,
            name_score: i64::from(basename_same(path, dst_path)),
            used: true,
        };
        let mut worst = 0;
        for k in 1..slots.len() {
            if compare(&slots[k], &slots[worst]) > 0 {
                worst = k;
            }
        }
        if compare(&slots[worst], &this) > 0 {
            slots[worst] = this;
        }
    }
    let mut sorted = slots.to_vec();
    sorted.sort_by(|a, b| compare(a, b).cmp(&0));
    Ok(sorted
        .first()
        .filter(|s| s.used && s.score >= MIN_SCORE as i64)
        .map(|s| s.src))
}

/// The object id of `data` as a blob, for comparing a work tree file with committed blobs.
pub(super) fn blob_id(git: &GitHome, data: &[u8]) -> Result<ObjectId> {
    Ok(pyn_object::hash_object(
        git.algorithm(),
        pyn_object::Kind::Blob,
        data,
    )?)
}

use std::collections::HashMap;

use crate::error::{Error, Result, corrupt};

fn varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let b = *data
            .get(*pos)
            .ok_or_else(|| corrupt("delta", "truncated header"))?;
        *pos += 1;
        if shift > 63 {
            return Err(corrupt("delta", "size overflow"));
        }
        value |= u64::from(b & 0x7f) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
}

/// The base and result sizes a delta declares.
pub(crate) fn sizes(delta: &[u8]) -> Result<(u64, u64)> {
    let mut pos = 0;
    let src = varint(delta, &mut pos)?;
    Ok((src, varint(delta, &mut pos)?))
}

/// Applies a git delta (copy and insert instructions) to `base`.
pub(crate) fn apply(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
    apply_capped(base, delta, u64::MAX)
}

/// Like [`apply`], but rejects a declared result size above `max_out` before allocating.
pub(crate) fn apply_capped(base: &[u8], delta: &[u8], max_out: u64) -> Result<Vec<u8>> {
    let mut pos = 0;
    let src = varint(delta, &mut pos)?;
    let dst = varint(delta, &mut pos)?;
    if dst > max_out {
        return Err(Error::Limit {
            what: "delta result size",
            limit: max_out,
        });
    }
    if src != base.len() as u64 {
        return Err(corrupt("delta", "base size does not match"));
    }
    let mut out = Vec::with_capacity(dst.min(64 << 20) as usize);
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            let mut offset = 0u64;
            let mut size = 0u64;
            for i in 0..4 {
                if op & (1 << i) != 0 {
                    let b = *delta
                        .get(pos)
                        .ok_or_else(|| corrupt("delta", "truncated copy"))?;
                    pos += 1;
                    offset |= u64::from(b) << (8 * i);
                }
            }
            for i in 0..3 {
                if op & (0x10 << i) != 0 {
                    let b = *delta
                        .get(pos)
                        .ok_or_else(|| corrupt("delta", "truncated copy"))?;
                    pos += 1;
                    size |= u64::from(b) << (8 * i);
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            let end = offset
                .checked_add(size)
                .filter(|e| *e <= base.len() as u64)
                .ok_or_else(|| corrupt("delta", "copy outside the base"))?;
            out.extend_from_slice(&base[offset as usize..end as usize]);
        } else if op != 0 {
            let n = usize::from(op);
            let chunk = delta
                .get(pos..pos.saturating_add(n))
                .ok_or_else(|| corrupt("delta", "truncated insert"))?;
            out.extend_from_slice(chunk);
            pos += n;
        } else {
            return Err(corrupt("delta", "reserved opcode 0"));
        }
        if out.len() as u64 > dst {
            return Err(corrupt("delta", "result larger than declared"));
        }
    }
    if out.len() as u64 != dst {
        return Err(corrupt("delta", "result size does not match"));
    }
    Ok(out)
}

const BLOCK: usize = 16;
const BUCKET: usize = 4;
const MAX_COPY: usize = 0x10000;
const RADIX: u32 = 0x0100_0193;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn block_hash(block: &[u8]) -> u32 {
    block.iter().fold(0u32, |h, b| {
        h.wrapping_mul(RADIX).wrapping_add(u32::from(*b))
    })
}

fn put_insert(out: &mut Vec<u8>, data: &[u8]) {
    for chunk in data.chunks(127) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
}

fn put_copy(out: &mut Vec<u8>, mut offset: usize, mut len: usize) {
    while len > 0 {
        let n = len.min(MAX_COPY);
        let at = out.len();
        out.push(0x80);
        for i in 0..4 {
            let b = (offset >> (8 * i)) as u8;
            if b != 0 {
                out[at] |= 1 << i;
                out.push(b);
            }
        }
        if n != MAX_COPY {
            for i in 0..3 {
                let b = (n >> (8 * i)) as u8;
                if b != 0 {
                    out[at] |= 0x10 << i;
                    out.push(b);
                }
            }
        }
        offset += n;
        len -= n;
    }
}

/// A git delta turning `base` into `target`, or `None` when it would not be shorter than `max` bytes.
pub(crate) fn create(base: &[u8], target: &[u8], max: usize) -> Option<Vec<u8>> {
    if base.len() < BLOCK || target.len() < BLOCK || u32::try_from(base.len()).is_err() {
        return None;
    }
    let mut index: HashMap<u32, Vec<u32>> = HashMap::new();
    for off in (0..=base.len() - BLOCK).step_by(BLOCK) {
        let bucket = index
            .entry(block_hash(&base[off..off + BLOCK]))
            .or_default();
        if bucket.len() < BUCKET {
            bucket.push(off as u32);
        }
    }
    let mut out = Vec::new();
    put_varint(&mut out, base.len() as u64);
    put_varint(&mut out, target.len() as u64);
    let top = (0..BLOCK - 1).fold(1u32, |p, _| p.wrapping_mul(RADIX));
    let mut i = 0;
    let mut pending = 0;
    let mut hash = block_hash(&target[..BLOCK]);
    while i + BLOCK <= target.len() {
        let mut best = (0, 0);
        for &off in index.get(&hash).into_iter().flatten() {
            let off = off as usize;
            let len = base[off..]
                .iter()
                .zip(&target[i..])
                .take_while(|(a, b)| a == b)
                .count();
            if len >= BLOCK && len > best.1 {
                best = (off, len);
            }
        }
        if best.1 >= BLOCK {
            let (mut off, mut len) = best;
            while i > pending && off > 0 && base[off - 1] == target[i - 1] {
                i -= 1;
                off -= 1;
                len += 1;
            }
            put_insert(&mut out, &target[pending..i]);
            put_copy(&mut out, off, len);
            i += len;
            pending = i;
            if i + BLOCK <= target.len() {
                hash = block_hash(&target[i..i + BLOCK]);
            }
        } else {
            if i + BLOCK < target.len() {
                hash = hash
                    .wrapping_sub(u32::from(target[i]).wrapping_mul(top))
                    .wrapping_mul(RADIX)
                    .wrapping_add(u32::from(target[i + BLOCK]));
            }
            i += 1;
        }
        if out.len() >= max {
            return None;
        }
    }
    put_insert(&mut out, &target[pending..]);
    (out.len() < max).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_and_insert() {
        let base = b"hello world";
        // src 11, dst 9: copy offset 6 size 5 ("world"), insert "!!!!"
        let delta = [11, 9, 0x91, 6, 5, 4, b'!', b'!', b'!', b'!'];
        assert_eq!(apply(base, &delta).unwrap(), b"world!!!!");
    }

    #[test]
    fn rejects_bad_deltas() {
        let base = b"abc";
        assert!(apply(base, &[4, 1, 1, b'x']).is_err());
        assert!(apply(base, &[3, 1, 0x91, 5, 9]).is_err());
        assert!(apply(base, &[3, 2, 1, b'x']).is_err());
        assert!(apply(base, &[3, 1, 0]).is_err());
        assert!(apply(base, &[3, 3, 3, b'x']).is_err());
    }

    #[test]
    fn declared_size_above_cap_is_rejected_before_allocation() {
        // dst = u64::MAX as a varint
        let mut d = vec![3];
        d.extend([0xff; 9]);
        d.push(0x01);
        assert!(matches!(
            apply_capped(b"abc", &d, 1 << 20),
            Err(Error::Limit { .. })
        ));
        assert!(apply(b"abc", &d).is_err());
    }

    #[test]
    fn created_deltas_apply_back() {
        let base: Vec<u8> = (0..5000u32).flat_map(|n| n.to_le_bytes()).collect();
        let mut target = base.clone();
        target.splice(1000..1000, b"inserted text here".iter().copied());
        target.truncate(target.len() - 700);
        target.extend_from_slice(b"tail that is new");
        let d = create(&base, &target, target.len()).unwrap();
        assert!(d.len() < target.len() / 10);
        assert_eq!(apply(&base, &d).unwrap(), target);
        assert!(create(&base, &[7u8; 4000], 100).is_none());
        let long: Vec<u8> = vec![5; 300_000];
        let d = create(&long, &long, long.len()).unwrap();
        assert_eq!(apply(&long, &d).unwrap(), long);
    }
}

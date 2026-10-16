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
}

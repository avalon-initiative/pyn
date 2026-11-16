//! The `GIT binary patch` body of a diff: each direction as a deflated literal or delta in base85.

use std::io::Write;

use anyhow::Result;
use flate2::{Compression, write::ZlibEncoder};

const ALPHABET: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";
const LINE_BYTES: usize = 52;

/// Forward then reverse section, each ending in a blank line.
pub(super) fn body(old: &[u8], new: &[u8]) -> Result<String> {
    let mut out = String::from("GIT binary patch\n");
    out.push_str(&section(old, new)?);
    out.push_str(&section(new, old)?);
    Ok(out)
}

fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(data)?;
    Ok(z.finish()?)
}

// A delta replaces the literal only when its deflated form is smaller.
fn section(base: &[u8], target: &[u8]) -> Result<String> {
    let literal = deflate(target)?;
    let delta = if base.is_empty() || target.is_empty() {
        None
    } else {
        pyn_git::create_delta(base, target, literal.len())
    };
    let (kind, size, data) = match delta {
        Some(d) => {
            let packed = deflate(&d)?;
            if packed.len() < literal.len() {
                ("delta", d.len(), packed)
            } else {
                ("literal", target.len(), literal)
            }
        }
        None => ("literal", target.len(), literal),
    };
    let mut out = format!("{kind} {size}\n");
    for chunk in data.chunks(LINE_BYTES) {
        let n = chunk.len();
        out.push(if n <= 26 {
            b'A' + n as u8 - 1
        } else {
            b'a' + n as u8 - 27
        } as char);
        for quad in chunk.chunks(4) {
            let mut bytes = [0u8; 4];
            bytes[..quad.len()].copy_from_slice(quad);
            let mut v = u32::from_be_bytes(bytes);
            let mut digits = [0u8; 5];
            for d in digits.iter_mut().rev() {
                *d = ALPHABET[(v % 85) as usize];
                v /= 85;
            }
            out.extend(digits.iter().map(|d| *d as char));
        }
        out.push('\n');
    }
    out.push('\n');
    Ok(out)
}

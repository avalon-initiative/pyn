//! Content-defined chunking shared by the server and the CLI, so both cut large files at the same places.

pub const SMALL_OBJECT: usize = 1 << 20;
pub const MIN_CHUNK: usize = 256 << 10;
const AVG_CHUNK: usize = 1 << 20;
pub const MAX_CHUNK: usize = 4 << 20;
/// Normalised chunking: a stricter mask before the average size, a looser one after it.
const MASK_SMALL: u64 = !0 << (64 - 22);
const MASK_LARGE: u64 = !0 << (64 - 18);

fn gear() -> &'static [u64; 256] {
    static TABLE: std::sync::OnceLock<[u64; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u64; 256];
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for slot in &mut table {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            *slot = z ^ (z >> 31);
        }
        table
    })
}

/// Length of the first chunk of `data`, which holds at least `MAX_CHUNK` bytes unless the stream ended.
pub fn cut_point(data: &[u8]) -> usize {
    let n = data.len().min(MAX_CHUNK);
    if n <= MIN_CHUNK {
        return n;
    }
    let table = gear();
    let mut fp = 0u64;
    for (i, &b) in data[..n].iter().enumerate().skip(MIN_CHUNK) {
        fp = (fp << 1).wrapping_add(table[b as usize]);
        let mask = if i < AVG_CHUNK {
            MASK_SMALL
        } else {
            MASK_LARGE
        };
        if fp & mask == 0 {
            return i + 1;
        }
    }
    n
}

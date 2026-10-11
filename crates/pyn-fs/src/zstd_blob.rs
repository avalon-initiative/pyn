//! zstd blobs on disk: a compressed object is stored as `<hash>.zst` beside where the raw one would be.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

const LEVEL: i32 = 3;
const PROBE_BYTES: usize = 64 * 1024;
const MIN_COMPRESSIBLE: u64 = 512;
/// Compress only when the probe saves at least this share (5%).
const MIN_SAVING_PERCENT: usize = 5;

/// Whether compressing the file looks worth it, judged on its first block.
pub fn worthwhile(path: &Path, len: u64) -> std::io::Result<bool> {
    if len < MIN_COMPRESSIBLE {
        return Ok(false);
    }
    let mut head = Vec::with_capacity(PROBE_BYTES);
    File::open(path)?
        .take(PROBE_BYTES as u64)
        .read_to_end(&mut head)?;
    let packed = zstd::bulk::compress(&head, LEVEL)?;
    Ok(packed.len() * 100 <= head.len() * (100 - MIN_SAVING_PERCENT))
}

/// Compresses `src` into `dst`, recording the content size in the frame header.
pub fn compress_file(src: &Path, dst: &Path, len: u64) -> std::io::Result<()> {
    let mut out = File::create(dst)?;
    let mut enc = zstd::Encoder::new(&mut out, LEVEL)?;
    enc.set_pledged_src_size(Some(len))?;
    std::io::copy(&mut File::open(src)?, &mut enc)?;
    enc.finish()?;
    out.sync_all()
}

/// The uncompressed size recorded in a blob's frame header.
pub fn content_size(path: &Path) -> std::io::Result<u64> {
    let mut head = [0u8; 18];
    let n = File::open(path)?.read(&mut head)?;
    match zstd::zstd_safe::get_frame_content_size(&head[..n]) {
        Ok(Some(size)) => Ok(size),
        _ => Err(std::io::Error::other(
            "compressed object has no content size",
        )),
    }
}

/// Writes the first `skip + take` bytes of the decompressed blob, dropping `skip`, to `out`.
pub fn decode_range(
    path: &Path,
    skip: u64,
    take: u64,
    out: &mut impl Write,
) -> std::io::Result<()> {
    let mut dec = zstd::Decoder::new(File::open(path)?)?;
    std::io::copy(&mut (&mut dec).take(skip), &mut std::io::sink())?;
    std::io::copy(&mut dec.take(take), out)?;
    Ok(())
}

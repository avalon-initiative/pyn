//! Content-defined chunking for large objects (decision #194): objects over `SMALL_OBJECT` bytes are split into
//! chunks stored as ordinary blobs, plus a manifest keyed by the whole content's hash.

use std::future::Future;
use std::io;
use std::ops::Range;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

use crate::error::{PynError, Result};
use crate::object::{ObjectRead, ObjectReader, ObjectStore, clamp_range};
use crate::types::ContentHash;

pub const SMALL_OBJECT: usize = 1 << 20;
const MIN_CHUNK: usize = 256 << 10;
const AVG_CHUNK: usize = 1 << 20;
const MAX_CHUNK: usize = 4 << 20;
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

/// The ordered chunks that make up one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub total: u64,
    pub chunks: Vec<(ContentHash, u64)>,
}

impl Manifest {
    pub fn encode(&self) -> Vec<u8> {
        let mut text = format!("pyn-manifest 1\n{}\n", self.total);
        for (hash, len) in &self.chunks {
            text.push_str(&format!("{hash} {len}\n"));
        }
        text.into_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let bad = || PynError::Storage("unreadable chunk manifest".into());
        let text = std::str::from_utf8(bytes).map_err(|_| bad())?;
        let mut lines = text.lines();
        if lines.next() != Some("pyn-manifest 1") {
            return Err(bad());
        }
        let total: u64 = lines.next().and_then(|l| l.parse().ok()).ok_or_else(bad)?;
        let mut chunks = Vec::new();
        for line in lines {
            let (hash, len) = line.split_once(' ').ok_or_else(bad)?;
            chunks.push((ContentHash::new(hash), len.parse().map_err(|_| bad())?));
        }
        if chunks.iter().map(|(_, len)| len).sum::<u64>() != total {
            return Err(bad());
        }
        Ok(Self { total, chunks })
    }

    /// The chunks overlapping `range`, each with the part of it to read.
    fn pieces(&self, range: &Range<u64>) -> Vec<(ContentHash, Range<u64>)> {
        let mut out = Vec::new();
        let mut offset = 0u64;
        for (hash, len) in &self.chunks {
            let (start, end) = (offset, offset + len);
            offset = end;
            if end <= range.start || start >= range.end {
                continue;
            }
            out.push((
                hash.clone(),
                range.start.max(start) - start..range.end.min(end) - start,
            ));
        }
        out
    }
}

type Opening = Pin<Box<dyn Future<Output = Result<Option<ObjectRead>>> + Send>>;

/// Reads a list of chunk pieces in order, opening one at a time.
struct ChainReader {
    store: Arc<dyn ObjectStore>,
    pieces: std::vec::IntoIter<(ContentHash, Range<u64>)>,
    opening: Option<Opening>,
    current: Option<ObjectReader>,
}

impl AsyncRead for ChainReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if let Some(reader) = self.current.as_mut() {
                let before = buf.filled().len();
                match reader.as_mut().poll_read(cx, buf) {
                    Poll::Ready(Ok(())) if buf.filled().len() == before => self.current = None,
                    other => return other,
                }
            } else if let Some(opening) = self.opening.as_mut() {
                match opening.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(Some(found))) => {
                        self.opening = None;
                        self.current = Some(found.reader);
                    }
                    Poll::Ready(Ok(None)) => {
                        self.opening = None;
                        return Poll::Ready(Err(io::Error::other(
                            "a chunk of the object is missing",
                        )));
                    }
                    Poll::Ready(Err(e)) => {
                        self.opening = None;
                        return Poll::Ready(Err(io::Error::other(e.to_string())));
                    }
                }
            } else if let Some((hash, range)) = self.pieces.next() {
                let store = self.store.clone();
                self.opening = Some(Box::pin(
                    async move { store.read(&hash, Some(range)).await },
                ));
            } else {
                return Poll::Ready(Ok(()));
            }
        }
    }
}

/// Splits large objects into deduplicated chunks over any store that can keep manifests.
pub struct ChunkedStore {
    inner: Arc<dyn ObjectStore>,
}

impl ChunkedStore {
    pub fn new(inner: Arc<dyn ObjectStore>) -> Self {
        Self { inner }
    }

    async fn manifest(&self, hash: &ContentHash) -> Result<Option<Manifest>> {
        self.inner
            .get_manifest(hash)
            .await?
            .map(|b| Manifest::decode(&b))
            .transpose()
    }
}

fn io_err(e: io::Error) -> PynError {
    PynError::Storage(e.to_string())
}

/// Reads into `buf` until it holds `want` bytes or the stream ends.
async fn fill(reader: &mut ObjectReader, buf: &mut Vec<u8>, want: usize) -> Result<()> {
    let mut chunk = vec![0u8; 256 << 10];
    while buf.len() < want {
        let n = reader.read(&mut chunk).await.map_err(io_err)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(())
}

#[async_trait]
impl ObjectStore for ChunkedStore {
    async fn put_stream(&self, mut reader: ObjectReader, max: Option<u64>) -> Result<ContentHash> {
        let mut buf = Vec::new();
        fill(&mut reader, &mut buf, SMALL_OBJECT + 1).await?;
        if buf.len() <= SMALL_OBJECT {
            if let Some(limit) = max
                && buf.len() as u64 > limit
            {
                return Err(PynError::ObjectTooLarge { limit });
            }
            return self.inner.put(buf).await;
        }
        let mut whole = Sha256::new();
        let mut total = 0u64;
        let mut chunks = Vec::new();
        loop {
            fill(&mut reader, &mut buf, MAX_CHUNK).await?;
            if buf.is_empty() {
                break;
            }
            let cut = cut_point(&buf);
            let rest = buf.split_off(cut);
            let chunk = std::mem::replace(&mut buf, rest);
            total += chunk.len() as u64;
            if let Some(limit) = max
                && total > limit
            {
                return Err(PynError::ObjectTooLarge { limit });
            }
            whole.update(&chunk);
            let len = chunk.len() as u64;
            chunks.push((self.inner.put(chunk).await?, len));
        }
        let hash = ContentHash::new(hex::encode(whole.finalize()));
        let manifest = Manifest { total, chunks };
        self.inner.put_manifest(&hash, manifest.encode()).await?;
        Ok(hash)
    }

    async fn read(
        &self,
        hash: &ContentHash,
        range: Option<Range<u64>>,
    ) -> Result<Option<ObjectRead>> {
        let Some(manifest) = self.manifest(hash).await? else {
            return self.inner.read(hash, range).await;
        };
        let range = clamp_range(range, manifest.total);
        let reader = ChainReader {
            store: self.inner.clone(),
            pieces: manifest.pieces(&range).into_iter(),
            opening: None,
            current: None,
        };
        Ok(Some(ObjectRead {
            reader: Box::pin(reader),
            total: manifest.total,
            range,
        }))
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool> {
        self.inner.exists(hash).await
    }

    async fn size(&self, hash: &ContentHash) -> Result<Option<u64>> {
        match self.manifest(hash).await? {
            Some(m) => Ok(Some(m.total)),
            None => self.inner.size(hash).await,
        }
    }

    async fn delete(&self, hash: &ContentHash) -> Result<bool> {
        self.inner.delete(hash).await
    }

    async fn chunks_of(&self, hash: &ContentHash) -> Result<Vec<ContentHash>> {
        Ok(self
            .manifest(hash)
            .await?
            .map(|m| m.chunks.into_iter().map(|(h, _)| h).collect())
            .unwrap_or_default())
    }
}

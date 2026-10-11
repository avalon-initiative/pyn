//! Content-addressed `ObjectStore` on the local filesystem. Compressible objects are stored zstd-compressed.

mod zstd_blob;

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use pyn_core::object::clamp_range;
use pyn_core::{ContentHash, ObjectRead, ObjectReader, ObjectStore, PynError, Result};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Objects live at `<root>/<first two hex chars>/<rest>`; writes go through a temp file and rename.
#[derive(Debug, Clone)]
pub struct FsObjectStore {
    root: PathBuf,
}

type Chunk = std::io::Result<bytes::Bytes>;

/// Sends what a blocking decoder writes to an async reader, so a decode error reaches the reader.
struct ChannelWriter(tokio::sync::mpsc::Sender<Chunk>);

impl std::io::Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .blocking_send(Ok(bytes::Bytes::copy_from_slice(buf)))
            .map_err(|_| std::io::Error::other("reader gone"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn tokio_stream_from(
    mut rx: tokio::sync::mpsc::Receiver<Chunk>,
) -> impl futures_core::Stream<Item = Chunk> {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

fn manifest_path(raw: &Path) -> PathBuf {
    let mut name = raw.file_name().expect("object path has a name").to_owned();
    name.push(".cm");
    raw.with_file_name(name)
}

fn packed_path(raw: &Path) -> PathBuf {
    let mut name = raw.file_name().expect("object path has a name").to_owned();
    name.push(".zst");
    raw.with_file_name(name)
}

fn storage(e: impl std::fmt::Display) -> PynError {
    PynError::Storage(e.to_string())
}

impl FsObjectStore {
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        tokio::fs::create_dir_all(&root).await.map_err(storage)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    async fn write_temp(
        &self,
        reader: &mut ObjectReader,
        max: Option<u64>,
        tmp: &Path,
    ) -> Result<ContentHash> {
        let mut file = tokio::fs::File::create(tmp).await.map_err(storage)?;
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf).await.map_err(storage)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if max.is_some_and(|limit| total > limit) {
                return Err(PynError::ObjectTooLarge {
                    limit: max.unwrap_or_default(),
                });
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).await.map_err(storage)?;
        }
        file.sync_all().await.map_err(storage)?;
        Ok(ContentHash::new(hex::encode(hasher.finalize())))
    }

    /// Moves the finished temp file into the store, compressed when that pays; leaves `tmp` for the caller to remove.
    async fn place(&self, hash: &ContentHash, tmp: &Path) -> Result<()> {
        let raw = self.path_for(hash)?;
        let packed = packed_path(&raw);
        if tokio::fs::try_exists(&raw).await.map_err(storage)?
            || tokio::fs::try_exists(&packed).await.map_err(storage)?
        {
            return Ok(());
        }
        tokio::fs::create_dir_all(raw.parent().expect("object path has a parent"))
            .await
            .map_err(storage)?;
        let len = tokio::fs::metadata(tmp).await.map_err(storage)?.len();
        let tmp_owned = tmp.to_path_buf();
        let compress = tokio::task::spawn_blocking(move || {
            zstd_blob::worthwhile(&tmp_owned, len).map_err(storage)
        })
        .await
        .map_err(storage)??;
        if !compress {
            return tokio::fs::rename(tmp, &raw).await.map_err(storage);
        }
        let staged = tmp.with_extension("zst");
        let (src, dst) = (tmp.to_path_buf(), staged.clone());
        let done = tokio::task::spawn_blocking(move || zstd_blob::compress_file(&src, &dst, len))
            .await
            .map_err(storage)?;
        if let Err(e) = done {
            let _ = tokio::fs::remove_file(&staged).await;
            return Err(storage(e));
        }
        tokio::fs::rename(&staged, &packed).await.map_err(storage)
    }

    /// Rejects anything that is not 64 lowercase hex characters, so a hash can never escape the root.
    fn path_for(&self, hash: &ContentHash) -> Result<PathBuf> {
        let h = hash.as_str();
        let valid = h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !valid {
            return Err(PynError::ObjectMissing(h.to_string()));
        }
        Ok(self.root.join(&h[..2]).join(&h[2..]))
    }
}

#[async_trait]
impl ObjectStore for FsObjectStore {
    async fn put_stream(&self, mut reader: ObjectReader, max: Option<u64>) -> Result<ContentHash> {
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join(format!(".tmp-{}-{n}", std::process::id()));
        let written = self.write_temp(&mut reader, max, &tmp).await;
        let hash = match written {
            Ok(hash) => hash,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
        };
        let placed = self.place(&hash, &tmp).await;
        let _ = tokio::fs::remove_file(&tmp).await;
        placed.map(|()| hash)
    }

    async fn read(
        &self,
        hash: &ContentHash,
        range: Option<Range<u64>>,
    ) -> Result<Option<ObjectRead>> {
        let path = self.path_for(hash)?;
        match tokio::fs::File::open(&path).await {
            Ok(mut file) => {
                let total = file.metadata().await.map_err(storage)?.len();
                let range = clamp_range(range, total);
                file.seek(std::io::SeekFrom::Start(range.start))
                    .await
                    .map_err(storage)?;
                return Ok(Some(ObjectRead {
                    reader: Box::pin(file.take(range.end - range.start)),
                    total,
                    range,
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(storage(e)),
        }
        let packed = packed_path(&path);
        let probe = packed.clone();
        let total = match tokio::task::spawn_blocking(move || zstd_blob::content_size(&probe))
            .await
            .map_err(storage)?
        {
            Ok(total) => total,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(storage(e)),
        };
        let range = clamp_range(range, total);
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let (skip, take) = (range.start, range.end - range.start);
        tokio::task::spawn_blocking(move || {
            let mut out = ChannelWriter(tx.clone());
            if let Err(e) = zstd_blob::decode_range(&packed, skip, take, &mut out) {
                let _ = tx.blocking_send(Err(e));
            }
        });
        let stream = tokio_stream_from(rx);
        Ok(Some(ObjectRead {
            reader: Box::pin(tokio_util::io::StreamReader::new(stream)),
            total,
            range,
        }))
    }

    async fn put_manifest(&self, key: &ContentHash, bytes: Vec<u8>) -> Result<()> {
        let dest = manifest_path(&self.path_for(key)?);
        tokio::fs::create_dir_all(dest.parent().expect("object path has a parent"))
            .await
            .map_err(storage)?;
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join(format!(".tmp-{}-{n}", std::process::id()));
        let written = async {
            let mut file = tokio::fs::File::create(&tmp).await?;
            file.write_all(&bytes).await?;
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(&tmp, &dest).await
        }
        .await;
        if written.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        written.map_err(storage)
    }

    async fn get_manifest(&self, key: &ContentHash) -> Result<Option<Vec<u8>>> {
        match tokio::fs::read(manifest_path(&self.path_for(key)?)).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage(e)),
        }
    }

    async fn delete(&self, hash: &ContentHash) -> Result<bool> {
        let path = self.path_for(hash)?;
        let mut removed = false;
        for target in [path.clone(), packed_path(&path), manifest_path(&path)] {
            match tokio::fs::remove_file(target).await {
                Ok(()) => removed = true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(storage(e)),
            }
        }
        Ok(removed)
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool> {
        let path = self.path_for(hash)?;
        for candidate in [path.clone(), packed_path(&path), manifest_path(&path)] {
            if tokio::fs::try_exists(candidate).await.map_err(storage)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn size(&self, hash: &ContentHash) -> Result<Option<u64>> {
        let path = self.path_for(hash)?;
        match tokio::fs::metadata(&path).await {
            Ok(meta) => return Ok(Some(meta.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(storage(e)),
        }
        let packed = packed_path(&path);
        match tokio::task::spawn_blocking(move || zstd_blob::content_size(&packed))
            .await
            .map_err(storage)?
        {
            Ok(size) => Ok(Some(size)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage(e)),
        }
    }
}

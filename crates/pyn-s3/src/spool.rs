//! Holds content while it is hashed: memory up to a threshold, then a temporary file that is removed on drop.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use pyn_core::{PynError, Result};
use tokio::io::AsyncWriteExt;

pub(crate) const SPOOL_PREFIX: &str = ".s3spool-";

fn storage(e: impl std::fmt::Display) -> PynError {
    PynError::Storage(format!("spool: {e}"))
}

pub(crate) struct TempFile(PathBuf);

impl TempFile {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

enum State {
    Memory(Vec<u8>),
    File {
        file: tokio::fs::File,
        temp: TempFile,
    },
}

pub(crate) struct Spool {
    dir: PathBuf,
    threshold: usize,
    state: State,
    len: u64,
}

/// Finished content, ready to send.
pub(crate) enum Content {
    Memory(Bytes),
    File(TempFile),
}

impl Spool {
    pub fn new(dir: &Path, threshold: usize) -> Self {
        Self {
            dir: dir.to_owned(),
            threshold,
            state: State::Memory(Vec::new()),
            len: 0,
        }
    }

    pub async fn write(&mut self, data: &[u8]) -> Result<()> {
        if let State::Memory(mem) = &mut self.state
            && mem.len() + data.len() > self.threshold
        {
            let mut id = [0u8; 16];
            getrandom::fill(&mut id).map_err(|e| storage(format!("no randomness: {e}")))?;
            let path = self.dir.join(format!("{SPOOL_PREFIX}{}", hex::encode(id)));
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
                .map_err(storage)?;
            let temp = TempFile(path);
            file.write_all(mem).await.map_err(storage)?;
            self.state = State::File { file, temp };
        }
        match &mut self.state {
            State::Memory(mem) => mem.extend_from_slice(data),
            State::File { file, .. } => file.write_all(data).await.map_err(storage)?,
        }
        self.len += data.len() as u64;
        Ok(())
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub async fn finish(self) -> Result<Content> {
        match self.state {
            State::Memory(mem) => Ok(Content::Memory(Bytes::from(mem))),
            State::File { mut file, temp } => {
                file.flush().await.map_err(storage)?;
                Ok(Content::File(temp))
            }
        }
    }
}

/// Removes spool files in `dir` last modified more than `age` ago; returns how many.
pub(crate) async fn sweep(dir: &Path, age: Duration) -> Result<usize> {
    let mut entries = tokio::fs::read_dir(dir).await.map_err(storage)?;
    let mut removed = 0;
    while let Some(entry) = entries.next_entry().await.map_err(storage)? {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(SPOOL_PREFIX)
        {
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|d| d > age);
        if meta.is_file() && old && tokio::fs::remove_file(entry.path()).await.is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

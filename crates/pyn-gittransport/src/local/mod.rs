//! A local repository served in process: the same upload-pack and receive-pack exchanges a remote
//! speaks, answered from the repository's own object store and refs, with no child process.

mod receive;
mod upload;

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

use pyn_git::{Config, Repository};
use pyn_gitwire::pkt::{MAX_DATA, encode_data};
use pyn_gitwire::{Error, ProtocolVersion, Result, Service, Session, Transport};
use pyn_object::{Algorithm, ObjectId};

use crate::env::Env;

/// Largest payload of a sideband packet: the band byte takes one byte of the pkt-line.
const BAND_DATA: usize = MAX_DATA - 1;

#[derive(Debug, Clone)]
pub struct LocalTransport {
    path: PathBuf,
    shown: String,
    env: Env,
}

impl LocalTransport {
    /// `shown` is how errors name the remote.
    pub fn new(path: PathBuf, shown: String, env: &Env) -> Self {
        Self {
            path,
            shown,
            env: env.clone(),
        }
    }

    fn open_repo(&self) -> Result<Repository> {
        let not_a_repo = || {
            Error::Connection(format!(
                "'{}' does not appear to be a git repository",
                self.shown
            ))
        };
        let meta = std::fs::metadata(&self.path).map_err(|_| not_a_repo())?;
        if !meta.is_dir() {
            return Err(not_a_repo());
        }
        let with_suffix = PathBuf::from(format!("{}.git", self.path.display()));
        let repo = [self.path.as_path(), with_suffix.as_path()]
            .into_iter()
            .find_map(|p| Repository::open(p).ok())
            .ok_or_else(not_a_repo)?;
        let unreadable = |e: &dyn std::fmt::Display| {
            Error::Connection(format!("cannot read the objects of '{}': {e}", self.shown))
        };
        let objects = repo.objects().map_err(|e| unreadable(&e))?;
        std::fs::read_dir(objects.dir()).map_err(|e| unreadable(&e))?;
        Ok(repo)
    }

    /// The server-side view of configuration: the user's global files under the repository's own.
    fn config(&self, repo: &Repository) -> Result<Config> {
        let mut config = crate::user_config(&self.env).map_err(Error::from)?;
        config.extend(repo.config().clone());
        Ok(config)
    }
}

impl Transport for LocalTransport {
    fn open(&mut self, service: Service, _version: ProtocolVersion) -> Result<Box<dyn Session>> {
        let repo = self.open_repo()?;
        let config = self.config(&repo)?;
        let advertisement = match service {
            Service::UploadPack => upload::advertisement(&repo, &config)?,
            Service::ReceivePack => receive::advertisement(&repo, &config)?,
        };
        Ok(Box::new(LocalSession {
            repo,
            config,
            service,
            advertisement: Some(advertisement),
            reply: Box::new(std::io::empty()),
        }))
    }
}

struct LocalSession {
    repo: Repository,
    config: Config,
    service: Service,
    advertisement: Option<Vec<u8>>,
    reply: Box<dyn Read>,
}

impl Session for LocalSession {
    fn advertisement(&mut self) -> Result<Box<dyn Read + '_>> {
        let bytes = self.advertisement.take().unwrap_or_default();
        Ok(Box::new(std::io::Cursor::new(bytes)))
    }

    fn send(&mut self, body: &mut dyn Read) -> Result<()> {
        self.reply = match self.service {
            Service::UploadPack => upload::serve(&self.repo, &self.config, body)?,
            Service::ReceivePack => receive::serve(&self.repo, &self.config, body)?,
        };
        Ok(())
    }

    fn response(&mut self) -> Result<Box<dyn Read + '_>> {
        Ok(Box::new(&mut *self.reply))
    }
}

pub(crate) fn parse_id(algorithm: Algorithm, hex: &str) -> crate::Result<ObjectId> {
    Ok(ObjectId::from_hex(algorithm, hex).map_err(Error::Id)?)
}

pub(crate) fn zero_id(algorithm: Algorithm) -> ObjectId {
    ObjectId::from_bytes(algorithm, &vec![0; algorithm.digest_len()]).expect("digest length")
}

/// Config that hides refs from a client, which these exchanges do not honour.
pub(crate) fn refuse_hidden_refs(config: &Config, keys: &[&str]) -> Result<()> {
    match keys.iter().find(|k| config.get(k).is_some()) {
        Some(k) => Err(Error::Unsupported(format!("the {k} setting"))),
        None => Ok(()),
    }
}

pub(crate) fn scratch() -> Result<File> {
    tempfile::tempfile().map_err(Error::Io)
}

/// A sideband band 1 writer: each chunk of what is written becomes one packet.
pub(crate) struct BandWriter<W: Write> {
    out: W,
    band: u8,
    buf: Vec<u8>,
}

impl<W: Write> BandWriter<W> {
    pub(crate) fn new(out: W, band: u8) -> Self {
        Self {
            out,
            band,
            buf: Vec::new(),
        }
    }

    fn emit(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let mut data = vec![self.band];
        data.append(&mut self.buf);
        let mut pkt = Vec::with_capacity(data.len() + 4);
        encode_data(&mut pkt, &data).map_err(|e| std::io::Error::other(e.to_string()))?;
        self.out.write_all(&pkt)
    }

    pub(crate) fn finish(mut self) -> std::io::Result<W> {
        self.emit()?;
        Ok(self.out)
    }
}

impl<W: Write> Write for BandWriter<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let room = BAND_DATA - self.buf.len();
        let take = room.min(data.len());
        self.buf.extend_from_slice(&data[..take]);
        if self.buf.len() == BAND_DATA {
            self.emit()?;
        }
        Ok(take)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_writer_splits_at_the_packet_limit() {
        let mut out = Vec::new();
        let mut w = BandWriter::new(&mut out, 1);
        w.write_all(&vec![7u8; BAND_DATA + 10]).unwrap();
        w.finish().unwrap();
        assert_eq!(&out[..4], b"fff0");
        assert_eq!(out[4], 1);
        let second = &out[MAX_DATA + 4..];
        assert_eq!(&second[..4], b"000f");
        assert_eq!(second[4], 1);
        assert_eq!(second.len(), 15);
    }
}

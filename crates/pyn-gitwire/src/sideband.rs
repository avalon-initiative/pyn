//! side-band-64k demultiplexing: channel 1 is payload, 2 is progress text, 3 is a fatal message.

use std::io::{self, Read};

use crate::error::Error;
use crate::pkt::{Packet, PktReader, text};

pub type ProgressFn<'a> = Box<dyn FnMut(&[u8]) + 'a>;

/// Yields channel 1 bytes; channel 2 goes to the callback, channel 3 fails the read with
/// [`Error::Remote`] (recoverable through `Error::from`). A flush packet is the end of the stream.
pub struct SidebandReader<'a, R: Read> {
    pkts: PktReader<R>,
    buf: Vec<u8>,
    pos: usize,
    done: bool,
    progress: Option<ProgressFn<'a>>,
}

impl<'a, R: Read> SidebandReader<'a, R> {
    pub fn new(pkts: PktReader<R>, progress: Option<ProgressFn<'a>>) -> Self {
        Self {
            pkts,
            buf: Vec::new(),
            pos: 0,
            done: false,
            progress,
        }
    }

    /// Starts from a packet that was already read off the stream.
    pub fn with_first(
        pkts: PktReader<R>,
        first: Packet,
        progress: Option<ProgressFn<'a>>,
    ) -> io::Result<Self> {
        let mut r = Self::new(pkts, progress);
        r.accept(first)?;
        Ok(r)
    }

    fn accept(&mut self, p: Packet) -> io::Result<()> {
        match p {
            Packet::Flush => self.done = true,
            Packet::Data(d) => match d.split_first() {
                Some((1, rest)) => {
                    self.buf.clear();
                    self.buf.extend_from_slice(rest);
                    self.pos = 0;
                }
                Some((2, rest)) => {
                    if let Some(cb) = self.progress.as_mut() {
                        cb(rest);
                    }
                }
                Some((3, rest)) => {
                    let msg = text(rest)
                        .map(str::to_owned)
                        .unwrap_or_else(|_| String::from_utf8_lossy(rest).into_owned());
                    return Err(Error::Remote(msg).into());
                }
                Some((c, _)) => {
                    return Err(Error::Protocol(format!("unknown sideband channel {c}")).into());
                }
                None => return Err(Error::Protocol("empty sideband packet".into()).into()),
            },
            other => {
                return Err(
                    Error::Protocol(format!("unexpected {other:?} in sideband stream")).into(),
                );
            }
        }
        Ok(())
    }
}

impl<R: Read> Read for SidebandReader<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.pos < self.buf.len() {
                let n = out.len().min(self.buf.len() - self.pos);
                out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            if self.done {
                return Ok(0);
            }
            let p = self.pkts.read()?;
            self.accept(p)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkt::PktWriter;

    fn stream(parts: &[(u8, &[u8])]) -> Vec<u8> {
        let mut v = Vec::new();
        let mut w = PktWriter::new(&mut v);
        for (c, d) in parts {
            let mut p = vec![*c];
            p.extend_from_slice(d);
            w.write_data(&p).unwrap();
        }
        w.write_flush().unwrap();
        v
    }

    #[test]
    fn demuxes_data_and_progress() {
        let s = stream(&[(2, b"counting"), (1, b"PA"), (2, b"more"), (1, b"CK")]);
        let mut seen = Vec::new();
        let mut r = SidebandReader::new(
            PktReader::new(s.as_slice()),
            Some(Box::new(|m| seen.push(m.to_vec()))),
        );
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        drop(r);
        assert_eq!(out, b"PACK");
        assert_eq!(seen, vec![b"counting".to_vec(), b"more".to_vec()]);
    }

    #[test]
    fn small_reads_cross_packets() {
        let s = stream(&[(1, b"abc"), (1, b"de")]);
        let mut r = SidebandReader::new(PktReader::new(s.as_slice()), None);
        let mut b = [0u8; 2];
        let mut out = Vec::new();
        loop {
            let n = r.read(&mut b).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&b[..n]);
        }
        assert_eq!(out, b"abcde");
    }

    #[test]
    fn channel_three_is_fatal() {
        let s = stream(&[(1, b"x"), (3, b"disk full\n")]);
        let mut r = SidebandReader::new(PktReader::new(s.as_slice()), None);
        let e = Error::from(r.read_to_end(&mut Vec::new()).unwrap_err());
        assert!(matches!(e, Error::Remote(m) if m == "disk full"));
    }

    #[test]
    fn unknown_channel_and_missing_flush() {
        let s = stream(&[(9, b"x")]);
        let mut r = SidebandReader::new(PktReader::new(s.as_slice()), None);
        assert!(matches!(
            Error::from(r.read(&mut [0; 4]).unwrap_err()),
            Error::Protocol(_)
        ));
        let mut t = stream(&[(1, b"x")]);
        t.truncate(t.len() - 4);
        let mut r = SidebandReader::new(PktReader::new(t.as_slice()), None);
        assert!(matches!(
            Error::from(r.read_to_end(&mut Vec::new()).unwrap_err()),
            Error::Pkt(_)
        ));
    }

    #[test]
    fn with_first_packet() {
        let s = stream(&[(1, b"two")]);
        let mut r = SidebandReader::with_first(
            PktReader::new(s.as_slice()),
            Packet::Data(b"\x01one".to_vec()),
            None,
        )
        .unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"onetwo");
    }
}

//! pkt-line framing: 4 hex digits of total length followed by the payload, with `0000` (flush), `0001`
//! (delim) and `0002` (response-end) as special packets.

use std::io::{self, Read, Write};

use crate::error::{Error, PktError, Result};

/// Largest packet including its 4-byte header.
pub const MAX_PKT: usize = 65520;
/// Largest payload of one data packet.
pub const MAX_DATA: usize = MAX_PKT - 4;

/// Most packets accepted in one advertisement, listing or report.
pub const MAX_LINES: usize = 1_000_000;
/// Most payload bytes buffered for one advertisement, listing or report.
pub const MAX_BUFFERED: usize = 256 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Data(Vec<u8>),
    Flush,
    Delim,
    ResponseEnd,
}

/// Strips one trailing newline.
pub fn trim_lf(data: &[u8]) -> &[u8] {
    data.strip_suffix(b"\n").unwrap_or(data)
}

/// A data payload as text without its trailing newline.
pub fn text(data: &[u8]) -> Result<&str> {
    std::str::from_utf8(trim_lf(data))
        .map_err(|_| Error::Protocol("packet is not valid UTF-8".into()))
}

pub fn encode_data(out: &mut Vec<u8>, data: &[u8]) -> std::result::Result<(), PktError> {
    if data.len() > MAX_DATA {
        return Err(PktError::TooLong(data.len()));
    }
    out.extend_from_slice(format!("{:04x}", data.len() + 4).as_bytes());
    out.extend_from_slice(data);
    Ok(())
}

fn parse_len(h: [u8; 4]) -> std::result::Result<usize, PktError> {
    let mut n = 0usize;
    for b in h {
        let d = (b as char).to_digit(16).ok_or(PktError::InvalidLength(h))?;
        n = n * 16 + d as usize;
    }
    Ok(n)
}

/// Decodes one packet from the front of `buf`, returning it and the bytes consumed.
pub fn decode(buf: &[u8]) -> Result<(Packet, usize)> {
    let mut r = buf;
    let p = PktReader::new(&mut r).read()?;
    Ok((p, buf.len() - r.len()))
}

/// Reads packets without buffering past the one requested, so the stream stays usable afterwards.
/// A data packet starting with `ERR ` becomes [`Error::Remote`].
pub struct PktReader<R> {
    inner: R,
}

impl<R: Read> PktReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    fn fill(&mut self, buf: &mut [u8]) -> Result<()> {
        self.inner.read_exact(buf).map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => PktError::UnexpectedEof.into(),
            _ => Error::from(e),
        })
    }

    pub fn read(&mut self) -> Result<Packet> {
        let mut header = [0u8; 4];
        self.fill(&mut header)?;
        match parse_len(header)? {
            0 => Ok(Packet::Flush),
            1 => Ok(Packet::Delim),
            2 => Ok(Packet::ResponseEnd),
            3 => Err(PktError::ReservedLength.into()),
            n if n > MAX_PKT => Err(PktError::InvalidLength(header).into()),
            n => {
                let mut data = vec![0u8; n - 4];
                self.fill(&mut data)?;
                if let Some(msg) = data.strip_prefix(b"ERR ") {
                    return Err(Error::Remote(
                        String::from_utf8_lossy(trim_lf(msg)).into_owned(),
                    ));
                }
                Ok(Packet::Data(data))
            }
        }
    }

    /// Reads data packets until the next flush, which is consumed, within [`MAX_LINES`] and [`MAX_BUFFERED`].
    pub fn read_until_flush(&mut self) -> Result<Vec<Vec<u8>>> {
        self.read_until_flush_within(MAX_LINES, MAX_BUFFERED)
    }

    /// Like [`PktReader::read_until_flush`] with explicit bounds.
    pub fn read_until_flush_within(
        &mut self,
        max_lines: usize,
        max_bytes: usize,
    ) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        let mut bytes = 0usize;
        loop {
            match self.read()? {
                Packet::Data(d) => {
                    bytes += d.len();
                    check_bounds(out.len() + 1, bytes, max_lines, max_bytes)?;
                    out.push(d);
                }
                Packet::Flush => return Ok(out),
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected {other:?} before flush"
                    )));
                }
            }
        }
    }
}

/// Fails with [`Error::LimitExceeded`] once `lines` or `bytes` pass the bounds.
pub(crate) fn check_bounds(
    lines: usize,
    bytes: usize,
    max_lines: usize,
    max_bytes: usize,
) -> Result<()> {
    if lines > max_lines {
        return Err(Error::LimitExceeded {
            what: "number of lines",
            limit: max_lines as u64,
        });
    }
    if bytes > max_bytes {
        return Err(Error::LimitExceeded {
            what: "buffered bytes",
            limit: max_bytes as u64,
        });
    }
    Ok(())
}

pub struct PktWriter<W> {
    inner: W,
}

impl<W: Write> PktWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }

    pub fn write_data(&mut self, data: &[u8]) -> Result<()> {
        let mut buf = Vec::with_capacity(data.len() + 4);
        encode_data(&mut buf, data)?;
        self.inner.write_all(&buf)?;
        Ok(())
    }

    /// Writes `line` plus a newline as one data packet.
    pub fn write_line(&mut self, line: &str) -> Result<()> {
        let mut data = Vec::with_capacity(line.len() + 1);
        data.extend_from_slice(line.as_bytes());
        data.push(b'\n');
        self.write_data(&data)
    }

    pub fn write_flush(&mut self) -> Result<()> {
        Ok(self.inner.write_all(b"0000")?)
    }

    pub fn write_delim(&mut self) -> Result<()> {
        Ok(self.inner.write_all(b"0001")?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        encode_data(&mut v, data).unwrap();
        v
    }

    #[test]
    fn encodes_and_decodes_data() {
        let buf = frame(b"hello\n");
        assert_eq!(buf, b"000ahello\n");
        assert_eq!(
            decode(&buf).unwrap(),
            (Packet::Data(b"hello\n".to_vec()), 10)
        );
    }

    #[test]
    fn empty_data_packet_is_valid() {
        assert_eq!(decode(b"0004").unwrap(), (Packet::Data(vec![]), 4));
    }

    #[test]
    fn special_packets() {
        assert_eq!(decode(b"0000").unwrap().0, Packet::Flush);
        assert_eq!(decode(b"0001").unwrap().0, Packet::Delim);
        assert_eq!(decode(b"0002").unwrap().0, Packet::ResponseEnd);
    }

    #[test]
    fn max_size_boundary() {
        let ok = vec![b'a'; MAX_DATA];
        let buf = frame(&ok);
        assert_eq!(&buf[..4], b"fff0");
        assert_eq!(decode(&buf).unwrap().0, Packet::Data(ok));
        assert_eq!(
            encode_data(&mut Vec::new(), &vec![0; MAX_DATA + 1]),
            Err(PktError::TooLong(MAX_DATA + 1))
        );
        assert!(matches!(
            decode(b"fff1xxxx"),
            Err(Error::Pkt(PktError::InvalidLength(_)))
        ));
    }

    #[test]
    fn rejects_bad_headers() {
        assert!(matches!(
            decode(b"0003"),
            Err(Error::Pkt(PktError::ReservedLength))
        ));
        assert!(matches!(
            decode(b"00zz"),
            Err(Error::Pkt(PktError::InvalidLength(_)))
        ));
        assert!(matches!(
            decode(b"+00a"),
            Err(Error::Pkt(PktError::InvalidLength(_)))
        ));
    }

    #[test]
    fn truncated_input() {
        assert!(matches!(
            decode(b"00"),
            Err(Error::Pkt(PktError::UnexpectedEof))
        ));
        assert!(matches!(
            decode(b"000ahi"),
            Err(Error::Pkt(PktError::UnexpectedEof))
        ));
        assert!(matches!(
            decode(b""),
            Err(Error::Pkt(PktError::UnexpectedEof))
        ));
    }

    #[test]
    fn err_packet_is_remote_error() {
        let buf = frame(b"ERR access denied\n");
        match decode(&buf) {
            Err(Error::Remote(m)) => assert_eq!(m, "access denied"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn streaming_reader_does_not_overread() {
        let mut stream = Vec::new();
        let mut w = PktWriter::new(&mut stream);
        w.write_line("one").unwrap();
        w.write_flush().unwrap();
        w.write_delim().unwrap();
        stream.extend_from_slice(b"tail");
        let mut cur = io::Cursor::new(stream);
        let mut r = PktReader::new(&mut cur);
        assert_eq!(r.read().unwrap(), Packet::Data(b"one\n".to_vec()));
        assert_eq!(r.read().unwrap(), Packet::Flush);
        assert_eq!(r.read().unwrap(), Packet::Delim);
        let mut rest = Vec::new();
        cur.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"tail");
    }

    #[test]
    fn read_until_flush_is_bounded_by_lines_and_bytes() {
        let mut w = PktWriter::new(Vec::new());
        for _ in 0..5 {
            w.write_line("abcd").unwrap();
        }
        w.write_flush().unwrap();
        let buf = w.into_inner();
        let lines = PktReader::new(&buf[..])
            .read_until_flush_within(5, 1000)
            .unwrap();
        assert_eq!(lines.len(), 5);
        assert!(matches!(
            PktReader::new(&buf[..]).read_until_flush_within(4, 1000),
            Err(Error::LimitExceeded {
                what: "number of lines",
                ..
            })
        ));
        assert!(matches!(
            PktReader::new(&buf[..]).read_until_flush_within(10, 24),
            Err(Error::LimitExceeded {
                what: "buffered bytes",
                ..
            })
        ));
    }

    #[test]
    fn read_until_flush_collects() {
        let mut v = Vec::new();
        let mut w = PktWriter::new(&mut v);
        w.write_line("a").unwrap();
        w.write_line("b").unwrap();
        w.write_flush().unwrap();
        let mut r = PktReader::new(v.as_slice());
        assert_eq!(
            r.read_until_flush().unwrap(),
            vec![b"a\n".to_vec(), b"b\n".to_vec()]
        );
    }
}

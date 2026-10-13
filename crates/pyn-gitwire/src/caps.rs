//! Capability lists and the server's initial advertisement (protocol v0/v1 refs, or v2 capabilities).

use std::io::Read;

use pyn_object::{Algorithm, ObjectId};

use crate::error::{Error, Result};
use crate::pkt::{Packet, PktReader, text};

pub const AGENT: &str = concat!("agent=pyn/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolVersion {
    V0,
    V1,
    V2,
}

/// Ordered `name[=value]` capabilities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    entries: Vec<(String, Option<String>)>,
}

impl Capabilities {
    /// Parses space-separated tokens (the v0/v1 capability string).
    pub fn parse_tokens(s: &str) -> Self {
        let mut c = Self::default();
        s.split_ascii_whitespace().for_each(|t| c.push(t));
        c
    }

    pub fn push(&mut self, token: &str) {
        match token.split_once('=') {
            Some((k, v)) => self.entries.push((k.to_owned(), Some(v.to_owned()))),
            None => self.entries.push((token.to_owned(), None)),
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_deref())
    }

    /// Every value of a repeatable capability such as `symref`.
    pub fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.entries
            .iter()
            .filter(move |(k, _)| k == name)
            .filter_map(|(_, v)| v.as_deref())
    }

    /// The space-separated features in a value such as `fetch=shallow wait-for-done`.
    pub fn features(&self, name: &str) -> Vec<&str> {
        self.value(name)
            .map(|v| v.split_ascii_whitespace().collect())
            .unwrap_or_default()
    }

    /// The advertised `object-format`; servers that do not advertise one use SHA-1.
    pub fn object_format(&self) -> Result<Algorithm> {
        match self.value("object-format") {
            None => Ok(Algorithm::Sha1),
            Some(n) => Algorithm::from_name(n)
                .ok_or_else(|| Error::Unsupported(format!("object format {n}"))),
        }
    }

    pub fn agent(&self) -> Option<&str> {
        self.value("agent")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedRef {
    pub name: String,
    pub id: ObjectId,
    /// The `^{}` entry that followed the ref, for annotated tags.
    pub peeled: Option<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    pub version: ProtocolVersion,
    pub capabilities: Capabilities,
    /// Empty for v2, which lists refs through `ls-refs`.
    pub refs: Vec<AdvertisedRef>,
}

impl Advertisement {
    /// `HEAD`'s symref target from a v0/v1 `symref=HEAD:<ref>` capability.
    pub fn symref_target(&self, name: &str) -> Option<&str> {
        self.capabilities.values("symref").find_map(|v| {
            v.split_once(':')
                .filter(|(n, _)| *n == name)
                .map(|(_, t)| t)
        })
    }
}

/// Reads one advertisement, consuming through its terminating flush. Smart HTTP's
/// `# service=` preamble is skipped.
pub fn read_advertisement<R: Read>(r: R) -> Result<Advertisement> {
    let mut pkts = PktReader::new(r);
    let mut first = pkts.read()?;
    if matches!(&first, Packet::Data(d) if d.starts_with(b"# service=")) {
        if pkts.read()? != Packet::Flush {
            return Err(Error::Protocol(
                "service announcement not followed by flush".into(),
            ));
        }
        first = pkts.read()?;
    }
    let Packet::Data(first) = first else {
        return Ok(Advertisement {
            version: ProtocolVersion::V0,
            capabilities: Capabilities::default(),
            refs: vec![],
        });
    };
    match text(&first)? {
        "version 2" => {
            let mut capabilities = Capabilities::default();
            for line in pkts.read_until_flush()? {
                capabilities.push(text(&line)?);
            }
            Ok(Advertisement {
                version: ProtocolVersion::V2,
                capabilities,
                refs: vec![],
            })
        }
        "version 1" => {
            let mut adv = read_refs(&mut pkts, None)?;
            adv.version = ProtocolVersion::V1;
            Ok(adv)
        }
        _ => read_refs(&mut pkts, Some(first)),
    }
}

fn read_refs<R: Read>(pkts: &mut PktReader<R>, first: Option<Vec<u8>>) -> Result<Advertisement> {
    let mut capabilities = Capabilities::default();
    let mut refs: Vec<AdvertisedRef> = Vec::new();
    let mut pending = first;
    let mut seen_caps = false;
    loop {
        let line = match pending.take() {
            Some(l) => l,
            None => match pkts.read()? {
                Packet::Data(d) => d,
                Packet::Flush => break,
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected {other:?} in ref advertisement"
                    )));
                }
            },
        };
        let (head, caps) = match line.iter().position(|&b| b == 0) {
            Some(i) => (&line[..i], Some(&line[i + 1..])),
            None => (&line[..], None),
        };
        if let Some(c) = caps.filter(|_| !seen_caps) {
            capabilities = Capabilities::parse_tokens(text(c)?);
            seen_caps = true;
        }
        let head = text(head)?;
        let (hex, name) = head
            .split_once(' ')
            .ok_or_else(|| Error::Protocol(format!("bad ref line {head:?}")))?;
        if name == "capabilities^{}" {
            continue;
        }
        let id = ObjectId::parse(hex)?;
        if let Some(base) = name.strip_suffix("^{}") {
            match refs.last_mut() {
                Some(last) if last.name == base => last.peeled = Some(id),
                _ => {
                    return Err(Error::Protocol(format!(
                        "peeled entry {name} without its ref"
                    )));
                }
            }
        } else {
            refs.push(AdvertisedRef {
                name: name.to_owned(),
                id,
                peeled: None,
            });
        }
    }
    Ok(Advertisement {
        version: ProtocolVersion::V0,
        capabilities,
        refs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkt::PktWriter;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    fn build(f: impl FnOnce(&mut PktWriter<&mut Vec<u8>>)) -> Vec<u8> {
        let mut v = Vec::new();
        f(&mut PktWriter::new(&mut v));
        v
    }

    #[test]
    fn parses_v2() {
        let buf = build(|w| {
            for l in [
                "version 2",
                "agent=git/2.43.0",
                "ls-refs=unborn",
                "fetch=shallow wait-for-done",
                "server-option",
                "object-format=sha256",
                "object-info",
            ] {
                w.write_line(l).unwrap();
            }
            w.write_flush().unwrap();
        });
        let a = read_advertisement(buf.as_slice()).unwrap();
        assert_eq!(a.version, ProtocolVersion::V2);
        assert_eq!(a.capabilities.agent(), Some("git/2.43.0"));
        assert_eq!(a.capabilities.features("ls-refs"), vec!["unborn"]);
        assert_eq!(
            a.capabilities.features("fetch"),
            vec!["shallow", "wait-for-done"]
        );
        assert!(a.capabilities.has("server-option") && a.capabilities.has("object-info"));
        assert_eq!(a.capabilities.object_format().unwrap(), Algorithm::Sha256);
    }

    #[test]
    fn parses_v0_with_peeled_and_symref() {
        let buf = build(|w| {
            w.write_data(format!("{A} HEAD\0multi_ack side-band-64k symref=HEAD:refs/heads/main object-format=sha1 agent=git/2\n").as_bytes()).unwrap();
            w.write_line(&format!("{A} refs/heads/main")).unwrap();
            w.write_line(&format!("{B} refs/tags/v1")).unwrap();
            w.write_line(&format!("{A} refs/tags/v1^{{}}")).unwrap();
            w.write_flush().unwrap();
        });
        let a = read_advertisement(buf.as_slice()).unwrap();
        assert_eq!(a.version, ProtocolVersion::V0);
        assert_eq!(a.refs.len(), 3);
        assert_eq!(a.refs[2].peeled, Some(ObjectId::parse(A).unwrap()));
        assert_eq!(a.symref_target("HEAD"), Some("refs/heads/main"));
        assert!(a.capabilities.has("side-band-64k"));
        assert_eq!(a.capabilities.object_format().unwrap(), Algorithm::Sha1);
    }

    #[test]
    fn parses_empty_repository_and_http_preamble() {
        let zero = "0".repeat(40);
        let buf = build(|w| {
            w.write_line("# service=git-receive-pack").unwrap();
            w.write_flush().unwrap();
            w.write_data(
                format!("{zero} capabilities^{{}}\0report-status delete-refs\n").as_bytes(),
            )
            .unwrap();
            w.write_flush().unwrap();
        });
        let a = read_advertisement(buf.as_slice()).unwrap();
        assert!(a.refs.is_empty());
        assert!(a.capabilities.has("delete-refs"));
    }

    #[test]
    fn version_one_prefix_and_errors() {
        let buf = build(|w| {
            w.write_line("version 1").unwrap();
            w.write_data(format!("{A} refs/heads/x\0report-status\n").as_bytes())
                .unwrap();
            w.write_flush().unwrap();
        });
        let a = read_advertisement(buf.as_slice()).unwrap();
        assert_eq!(a.version, ProtocolVersion::V1);
        assert_eq!(a.refs.len(), 1);
        let err = build(|w| w.write_line("ERR no such repository").unwrap());
        assert!(matches!(
            read_advertisement(err.as_slice()),
            Err(Error::Remote(_))
        ));
    }

    #[test]
    fn unknown_object_format_is_unsupported() {
        let c = Capabilities::parse_tokens("object-format=sha3");
        assert!(matches!(c.object_format(), Err(Error::Unsupported(_))));
    }
}

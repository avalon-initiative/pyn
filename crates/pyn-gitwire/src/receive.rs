//! Pushing: the v0/v1 `receive-pack` client (protocol v2 has no push).

use std::io::{Cursor, Read};

use pyn_object::{Algorithm, ObjectId};

use crate::caps::{
    AGENT, AdvertisedRef, Advertisement, Capabilities, ProtocolVersion, read_advertisement,
};
use crate::error::{Error, Result};
use crate::pkt::{PktReader, PktWriter, text};
use crate::sideband::SidebandReader;
use crate::transport::{Service, Session, Transport};

/// One ref command. `old` is `None` to create, `new` is `None` to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefUpdate {
    pub name: String,
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
}

impl RefUpdate {
    pub fn create(name: impl Into<String>, new: ObjectId) -> Self {
        Self {
            name: name.into(),
            old: None,
            new: Some(new),
        }
    }

    pub fn update(name: impl Into<String>, old: ObjectId, new: ObjectId) -> Self {
        Self {
            name: name.into(),
            old: Some(old),
            new: Some(new),
        }
    }

    pub fn delete(name: impl Into<String>, old: ObjectId) -> Self {
        Self {
            name: name.into(),
            old: Some(old),
            new: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PushOptions {
    /// All ref updates succeed or none do.
    pub atomic: bool,
    pub quiet: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefStatus {
    Ok,
    /// The server's reason (`ng`), or the unpack error that failed every ref.
    Rejected(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefResult {
    pub name: String,
    pub status: RefStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushReport {
    /// `None` when the server unpacked the pack (or none was sent), else its error text.
    pub unpack_error: Option<String>,
    pub refs: Vec<RefResult>,
}

impl PushReport {
    pub fn is_ok(&self) -> bool {
        self.unpack_error.is_none() && self.refs.iter().all(|r| r.status == RefStatus::Ok)
    }
}

pub struct ReceivePack {
    session: Box<dyn Session>,
    advert: Advertisement,
    algorithm: Algorithm,
}

impl ReceivePack {
    /// Connects and checks the server's object format against `algorithm`.
    pub fn open(transport: &mut dyn Transport, algorithm: Algorithm) -> Result<Self> {
        let mut session = transport.open(Service::ReceivePack, ProtocolVersion::V0)?;
        let advert = read_advertisement(session.advertisement()?)?;
        if advert.version == ProtocolVersion::V2 {
            return Err(Error::Unsupported("push over protocol v2".into()));
        }
        let server = advert.capabilities.object_format()?;
        if server != algorithm {
            return Err(Error::ObjectFormatMismatch {
                server,
                local: algorithm,
            });
        }
        Ok(Self {
            session,
            advert,
            algorithm,
        })
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.advert.capabilities
    }

    /// The refs the server advertised.
    pub fn refs(&self) -> &[AdvertisedRef] {
        &self.advert.refs
    }

    /// Sends the commands, then `pack` (required unless every update is a delete), and parses the status report.
    /// `progress` receives sideband text, including hook output.
    pub fn push(
        &mut self,
        updates: &[RefUpdate],
        pack: Option<&mut dyn Read>,
        opts: &PushOptions,
        progress: &mut dyn FnMut(&[u8]),
    ) -> Result<PushReport> {
        if updates.is_empty() {
            return Err(Error::Usage("push needs at least one ref update".into()));
        }
        let deletes_only = updates.iter().all(|u| u.new.is_none());
        match (&pack, deletes_only) {
            (None, false) => {
                return Err(Error::Usage(
                    "a pack is required unless every update is a delete".into(),
                ));
            }
            (Some(_), true) => return Err(Error::Usage("a delete-only push sends no pack".into())),
            _ => {}
        }
        let caps = &self.advert.capabilities;
        let status_cap = ["report-status-v2", "report-status"]
            .into_iter()
            .find(|c| caps.has(c))
            .ok_or_else(|| Error::Unsupported("report-status".into()))?;
        if updates.iter().any(|u| u.new.is_none()) && !caps.has("delete-refs") {
            return Err(Error::Unsupported("delete-refs".into()));
        }
        if opts.atomic && !caps.has("atomic") {
            return Err(Error::Unsupported("atomic".into()));
        }
        let sideband = caps.has("side-band-64k");
        let mut tokens = vec![status_cap.to_owned(), AGENT.to_owned()];
        if sideband {
            tokens.push("side-band-64k".into());
        }
        for (on, name) in [
            (opts.atomic, "atomic"),
            (opts.quiet, "quiet"),
            (!deletes_only, "ofs-delta"),
        ] {
            if on && caps.has(name) {
                tokens.push(name.to_owned());
            }
        }
        if caps.has("object-format") {
            tokens.push(format!("object-format={}", self.algorithm.name()));
        }
        let zero = |id: Option<ObjectId>| {
            id.map(|i| i.to_hex())
                .unwrap_or_else(|| "0".repeat(self.algorithm.digest_len() * 2))
        };
        let mut w = PktWriter::new(Vec::new());
        for (i, u) in updates.iter().enumerate() {
            let line = format!("{} {} {}", zero(u.old), zero(u.new), u.name);
            if i == 0 {
                w.write_data(format!("{line}\0{}\n", tokens.join(" ")).as_bytes())?;
            } else {
                w.write_line(&line)?;
            }
        }
        w.write_flush()?;
        let commands = w.into_inner();
        let mut body: Box<dyn Read + '_> = match pack {
            Some(p) => Box::new(Cursor::new(commands).chain(p)),
            None => Box::new(Cursor::new(commands)),
        };
        self.session.send(&mut body)?;
        let resp = self.session.response()?;
        let status = if sideband {
            let mut out = Vec::new();
            SidebandReader::new(PktReader::new(resp), Some(Box::new(|m| progress(m))))
                .read_to_end(&mut out)?;
            out
        } else {
            let mut out = Vec::new();
            let mut r = PktReader::new(resp);
            while let crate::pkt::Packet::Data(d) = r.read()? {
                crate::pkt::encode_data(&mut out, &d)?;
            }
            out
        };
        parse_report(&status, updates)
    }
}

fn parse_report(buf: &[u8], updates: &[RefUpdate]) -> Result<PushReport> {
    let mut pkts = PktReader::new(buf);
    let lines = pkts.read_until_flush()?;
    let mut unpack_error = None;
    let mut refs = Vec::new();
    let mut saw_unpack = false;
    for l in &lines {
        let line = text(l)?;
        if let Some(s) = line.strip_prefix("unpack ") {
            saw_unpack = true;
            if s != "ok" {
                unpack_error = Some(s.to_owned());
            }
        } else if let Some(name) = line.strip_prefix("ok ") {
            refs.push(RefResult {
                name: name.to_owned(),
                status: RefStatus::Ok,
            });
        } else if let Some(rest) = line.strip_prefix("ng ") {
            let (name, reason) = rest.split_once(' ').unwrap_or((rest, ""));
            refs.push(RefResult {
                name: name.to_owned(),
                status: RefStatus::Rejected(reason.to_owned()),
            });
        } else if !line.starts_with("option ") {
            return Err(Error::Protocol(format!(
                "unexpected report-status line {line:?}"
            )));
        }
    }
    if !saw_unpack {
        return Err(Error::Protocol("report-status has no unpack line".into()));
    }
    if let Some(e) = &unpack_error
        && refs.is_empty()
    {
        refs = updates
            .iter()
            .map(|u| RefResult {
                name: u.name.clone(),
                status: RefStatus::Rejected(e.clone()),
            })
            .collect();
    }
    Ok(PushReport { unpack_error, refs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(lines: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        let mut w = PktWriter::new(&mut v);
        lines.iter().for_each(|l| w.write_line(l).unwrap());
        w.write_flush().unwrap();
        v
    }

    #[test]
    fn parses_ok_ng_and_v2_options() {
        let buf = report(&[
            "unpack ok",
            "ok refs/heads/a",
            "option refname refs/heads/a",
            "ng refs/heads/b non-fast-forward",
        ]);
        let r = parse_report(&buf, &[]).unwrap();
        assert_eq!(r.unpack_error, None);
        assert_eq!(r.refs[0].status, RefStatus::Ok);
        assert_eq!(
            r.refs[1],
            RefResult {
                name: "refs/heads/b".into(),
                status: RefStatus::Rejected("non-fast-forward".into())
            }
        );
        assert!(!r.is_ok());
    }

    #[test]
    fn unpack_failure_rejects_every_ref() {
        let id = ObjectId::parse(&"1".repeat(40)).unwrap();
        let ups = [RefUpdate::create("refs/heads/a", id)];
        let r = parse_report(&report(&["unpack index-pack abnormal exit"]), &ups).unwrap();
        assert_eq!(r.unpack_error.as_deref(), Some("index-pack abnormal exit"));
        assert!(matches!(r.refs[0].status, RefStatus::Rejected(_)));
    }

    #[test]
    fn missing_unpack_line_is_a_violation() {
        assert!(matches!(
            parse_report(&report(&["ok refs/heads/a"]), &[]),
            Err(Error::Protocol(_))
        ));
    }
}

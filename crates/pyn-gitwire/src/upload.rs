//! Fetching: protocol v2 `ls-refs` and `fetch`, with a v0/v1 fallback when the server does not speak v2.

use std::io::{Cursor, Read};

use pyn_object::{Algorithm, ObjectId};

use crate::caps::{AGENT, Advertisement, Capabilities, ProtocolVersion, read_advertisement};
use crate::error::{Error, Result};
use crate::pkt::{Packet, PktReader, PktWriter, text};
use crate::sideband::{ProgressFn, SidebandReader};
use crate::transport::{Service, Session, Transport};

const FIRST_BATCH: usize = 16;
const MAX_BATCH: usize = 4096;
const MAX_ROUNDS: usize = 256;
const MAX_V0_HAVES: usize = 1024;

/// The received pack, with sideband demultiplexed. Read it to the end (the flush) before
/// issuing another request on the same connection.
pub type PackReader<'a> = SidebandReader<'a, Box<dyn Read + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef {
    pub name: String,
    /// `None` for an unborn ref (v2 `unborn`).
    pub id: Option<ObjectId>,
    pub peeled: Option<ObjectId>,
    pub symref_target: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LsRefsOptions {
    /// Only refs starting with one of these; empty lists everything.
    pub prefixes: Vec<String>,
    pub symrefs: bool,
    pub peel: bool,
    /// Report an unborn `HEAD` when the server supports it.
    pub unborn: bool,
}

/// How a shallow fetch limits history. Servers take only one kind at a time (git also lets a date and exclusions combine).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deepen {
    /// Commits from each tip, or from the current shallow boundary when `relative`.
    pub depth: Option<u32>,
    pub relative: bool,
    /// Commits at or after this Unix time.
    pub since: Option<i64>,
    /// Commits not reachable from these refs.
    pub not: Vec<String>,
}

/// What a shallow fetch changed on the server's side of the boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShallowInfo {
    pub shallow: Vec<ObjectId>,
    pub unshallow: Vec<ObjectId>,
}

#[derive(Debug, Clone)]
pub struct FetchRequest {
    pub wants: Vec<ObjectId>,
    /// Commits the client holds as shallow.
    pub shallow: Vec<ObjectId>,
    pub deepen: Option<Deepen>,
    /// The pack may delta against objects the client has (it must complete the pack itself).
    pub thin_pack: bool,
    pub ofs_delta: bool,
    pub no_progress: bool,
    pub include_tag: bool,
    /// A partial-clone filter spec (`blob:none`); the server must advertise `filter`.
    pub filter: Option<String>,
}

impl FetchRequest {
    pub fn new(wants: Vec<ObjectId>) -> Self {
        Self {
            wants,
            shallow: Vec::new(),
            deepen: None,
            thin_pack: false,
            ofs_delta: true,
            no_progress: false,
            include_tag: false,
            filter: None,
        }
    }
}

/// Supplies commits the client already has, most useful first.
pub trait HaveSource {
    /// Up to `max` more candidates; empty when exhausted.
    fn next_batch(&mut self, max: usize) -> Vec<ObjectId>;

    /// Candidates the server confirmed it has, so the source can skip their ancestors.
    fn acknowledged(&mut self, _common: &[ObjectId]) {}
}

impl<F: FnMut(usize) -> Vec<ObjectId>> HaveSource for F {
    fn next_batch(&mut self, max: usize) -> Vec<ObjectId> {
        self(max)
    }
}

/// A clone: nothing to negotiate.
pub struct NoHaves;

impl HaveSource for NoHaves {
    fn next_batch(&mut self, _max: usize) -> Vec<ObjectId> {
        Vec::new()
    }
}

pub struct UploadPack {
    session: Box<dyn Session>,
    advert: Advertisement,
    algorithm: Algorithm,
    shallow_info: ShallowInfo,
}

impl UploadPack {
    /// Connects and checks the server's object format against `algorithm`.
    pub fn open(transport: &mut dyn Transport, algorithm: Algorithm) -> Result<Self> {
        let mut session = transport.open(Service::UploadPack, ProtocolVersion::V2)?;
        let advert = read_advertisement(session.advertisement()?)?;
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
            shallow_info: ShallowInfo::default(),
        })
    }

    pub fn version(&self) -> ProtocolVersion {
        self.advert.version
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.advert.capabilities
    }

    pub fn object_format(&self) -> Algorithm {
        self.algorithm
    }

    /// The boundary changes the last fetch reported; empty when it was not a shallow request.
    pub fn shallow_info(&self) -> &ShallowInfo {
        &self.shallow_info
    }

    /// Whether the server takes a `filter` argument on fetch.
    pub fn supports_filter(&self) -> bool {
        if self.advert.version == ProtocolVersion::V2 {
            self.advert
                .capabilities
                .features("fetch")
                .contains(&"filter")
        } else {
            self.advert.capabilities.has("filter")
        }
    }

    fn check_shallow(&self, req: &FetchRequest) -> Result<()> {
        if req.shallow.is_empty() && req.deepen.is_none() {
            return Ok(());
        }
        let ok = if self.advert.version == ProtocolVersion::V2 {
            self.advert
                .capabilities
                .features("fetch")
                .contains(&"shallow")
        } else {
            self.advert.capabilities.has("shallow")
        };
        if !ok {
            return Err(Error::Unsupported(
                "the server does not support shallow requests".into(),
            ));
        }
        let Some(d) = &req.deepen else { return Ok(()) };
        if self.advert.version != ProtocolVersion::V2 {
            for (used, cap) in [
                (d.since.is_some(), "deepen-since"),
                (!d.not.is_empty(), "deepen-not"),
                (d.relative, "deepen-relative"),
            ] {
                if used && !self.advert.capabilities.has(cap) {
                    return Err(Error::Unsupported(format!(
                        "the server does not support {cap}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn deepen_lines(req: &FetchRequest, v2: bool) -> Vec<String> {
        let mut out: Vec<String> = req.shallow.iter().map(|s| format!("shallow {s}")).collect();
        if let Some(d) = &req.deepen {
            if let Some(n) = d.depth {
                out.push(format!("deepen {n}"));
            }
            if d.relative && v2 {
                out.push("deepen-relative".into());
            }
            if let Some(t) = d.since {
                out.push(format!("deepen-since {t}"));
            }
            out.extend(d.not.iter().map(|r| format!("deepen-not {r}")));
        }
        out
    }

    pub fn ls_refs(&mut self, opts: &LsRefsOptions) -> Result<Vec<RemoteRef>> {
        if self.advert.version != ProtocolVersion::V2 {
            return Ok(self.ls_refs_v0(opts));
        }
        if !self.advert.capabilities.has("ls-refs") {
            return Err(Error::Unsupported("ls-refs".into()));
        }
        let mut w = PktWriter::new(Vec::new());
        self.command_header(&mut w, "ls-refs")?;
        if opts.peel {
            w.write_line("peel")?;
        }
        if opts.symrefs {
            w.write_line("symrefs")?;
        }
        if opts.unborn
            && self
                .advert
                .capabilities
                .features("ls-refs")
                .contains(&"unborn")
        {
            w.write_line("unborn")?;
        }
        for p in &opts.prefixes {
            w.write_line(&format!("ref-prefix {p}"))?;
        }
        w.write_flush()?;
        let body = w.into_inner();
        self.session.send(&mut Cursor::new(body))?;
        let lines = PktReader::new(self.session.response()?).read_until_flush()?;
        lines
            .iter()
            .map(|l| self.parse_ref_line(text(l)?))
            .collect()
    }

    fn parse_ref_line(&self, line: &str) -> Result<RemoteRef> {
        let mut parts = line.split(' ');
        let (first, name) = (parts.next().unwrap_or(""), parts.next());
        let name = name.ok_or_else(|| Error::Protocol(format!("bad ls-refs line {line:?}")))?;
        let id = if first == "unborn" {
            None
        } else {
            Some(ObjectId::from_hex(self.algorithm, first)?)
        };
        let mut r = RemoteRef {
            name: name.to_owned(),
            id,
            peeled: None,
            symref_target: None,
        };
        for attr in parts {
            if let Some(t) = attr.strip_prefix("symref-target:") {
                r.symref_target = Some(t.to_owned());
            } else if let Some(p) = attr.strip_prefix("peeled:") {
                r.peeled = Some(ObjectId::from_hex(self.algorithm, p)?);
            }
        }
        Ok(r)
    }

    fn ls_refs_v0(&self, opts: &LsRefsOptions) -> Vec<RemoteRef> {
        self.advert
            .refs
            .iter()
            .filter(|r| {
                opts.prefixes.is_empty()
                    || opts.prefixes.iter().any(|p| r.name.starts_with(p.as_str()))
            })
            .map(|r| RemoteRef {
                name: r.name.clone(),
                id: Some(r.id),
                peeled: opts.peel.then_some(r.peeled).flatten(),
                symref_target: opts
                    .symrefs
                    .then(|| self.advert.symref_target(&r.name).map(str::to_owned))
                    .flatten(),
            })
            .collect()
    }

    fn command_header(&self, w: &mut PktWriter<Vec<u8>>, command: &str) -> Result<()> {
        w.write_line(&format!("command={command}"))?;
        w.write_line(AGENT)?;
        if self.advert.capabilities.has("object-format") {
            w.write_line(&format!("object-format={}", self.algorithm.name()))?;
        }
        w.write_delim()
    }

    /// Negotiates and returns the pack stream. `progress` receives the server's sideband progress text.
    pub fn fetch<'a>(
        &'a mut self,
        req: &FetchRequest,
        haves: &mut dyn HaveSource,
        progress: ProgressFn<'a>,
    ) -> Result<PackReader<'a>> {
        if req.wants.is_empty() {
            return Err(Error::Usage("fetch needs at least one want".into()));
        }
        self.check_shallow(req)?;
        if req.filter.is_some() && !self.supports_filter() {
            return Err(Error::Unsupported(
                "the server does not support filtering".into(),
            ));
        }
        self.shallow_info = ShallowInfo::default();
        if self.advert.version == ProtocolVersion::V2 {
            self.fetch_v2(req, haves, progress)
        } else {
            self.fetch_v0(req, haves, progress)
        }
    }

    fn fetch_v2<'a>(
        &'a mut self,
        req: &FetchRequest,
        haves: &mut dyn HaveSource,
        progress: ProgressFn<'a>,
    ) -> Result<PackReader<'a>> {
        if !self.advert.capabilities.has("fetch") {
            return Err(Error::Unsupported("fetch".into()));
        }
        let mut common: Vec<ObjectId> = Vec::new();
        let mut batch = FIRST_BATCH;
        let mut round = 0;
        let mut exhausted = false;
        loop {
            round += 1;
            let fresh = if exhausted || round > MAX_ROUNDS {
                Vec::new()
            } else {
                haves.next_batch(batch)
            };
            let done = fresh.is_empty();
            exhausted |= done;
            let mut w = PktWriter::new(Vec::new());
            self.command_header(&mut w, "fetch")?;
            for (on, flag) in [
                (req.thin_pack, "thin-pack"),
                (req.ofs_delta, "ofs-delta"),
                (req.no_progress, "no-progress"),
                (req.include_tag, "include-tag"),
            ] {
                if on {
                    w.write_line(flag)?;
                }
            }
            for id in &req.wants {
                w.write_line(&format!("want {id}"))?;
            }
            for line in Self::deepen_lines(req, true) {
                w.write_line(&line)?;
            }
            if let Some(f) = &req.filter {
                w.write_line(&format!("filter {f}"))?;
            }
            for id in common.iter().chain(&fresh) {
                w.write_line(&format!("have {id}"))?;
            }
            if done {
                w.write_line("done")?;
            }
            w.write_flush()?;
            self.session.send(&mut Cursor::new(w.into_inner()))?;
            let mut acks = Vec::new();
            let mut info = ShallowInfo::default();
            if read_fetch_sections(
                &mut PktReader::new(self.session.response()?),
                self.algorithm,
                &mut acks,
                &mut info,
            )? {
                self.shallow_info = info;
                break;
            }
            if done {
                return Err(Error::Protocol("server sent no packfile after done".into()));
            }
            haves.acknowledged(&acks);
            for id in acks {
                if !common.contains(&id) {
                    common.push(id);
                }
            }
            batch = (batch * 2).min(MAX_BATCH);
        }
        Ok(SidebandReader::new(
            PktReader::new(self.session.response()?),
            Some(progress),
        ))
    }

    /// One round: wants, then haves, then `done`.
    fn fetch_v0<'a>(
        &'a mut self,
        req: &FetchRequest,
        haves: &mut dyn HaveSource,
        progress: ProgressFn<'a>,
    ) -> Result<PackReader<'a>> {
        let caps = &self.advert.capabilities;
        if !caps.has("side-band-64k") {
            return Err(Error::Unsupported("side-band-64k".into()));
        }
        let mut tokens = vec!["side-band-64k".to_owned(), AGENT.to_owned()];
        for (on, name) in [
            (req.thin_pack, "thin-pack"),
            (req.ofs_delta, "ofs-delta"),
            (req.no_progress, "no-progress"),
            (req.include_tag, "include-tag"),
        ] {
            if on && caps.has(name) {
                tokens.push(name.to_owned());
            }
        }
        if caps.has("object-format") {
            tokens.push(format!("object-format={}", self.algorithm.name()));
        }
        if req.filter.is_some() {
            tokens.push("filter".to_owned());
        }
        if let Some(d) = &req.deepen {
            for (on, name) in [
                (d.since.is_some(), "deepen-since"),
                (!d.not.is_empty(), "deepen-not"),
                (d.relative, "deepen-relative"),
            ] {
                if on {
                    tokens.push(name.to_owned());
                }
            }
        }
        let mut w = PktWriter::new(Vec::new());
        for (i, id) in req.wants.iter().enumerate() {
            if i == 0 {
                w.write_data(format!("want {id} {}\n", tokens.join(" ")).as_bytes())?;
            } else {
                w.write_line(&format!("want {id}"))?;
            }
        }
        for line in Self::deepen_lines(req, false) {
            w.write_line(&line)?;
        }
        if let Some(f) = &req.filter {
            w.write_line(&format!("filter {f}"))?;
        }
        w.write_flush()?;
        let mut sent = 0;
        while sent < MAX_V0_HAVES {
            let batch = haves.next_batch(MAX_V0_HAVES - sent);
            if batch.is_empty() {
                break;
            }
            sent += batch.len();
            for id in batch {
                w.write_line(&format!("have {id}"))?;
            }
        }
        w.write_line("done")?;
        self.session.send(&mut Cursor::new(w.into_inner()))?;
        let mut pkts = PktReader::new(self.session.response()?);
        if req.deepen.is_some() {
            let mut info = ShallowInfo::default();
            loop {
                match pkts.read()? {
                    Packet::Data(d) => {
                        let line = text(&d)?;
                        if let Some(h) = line.strip_prefix("shallow ") {
                            info.shallow.push(ObjectId::from_hex(self.algorithm, h)?);
                        } else if let Some(h) = line.strip_prefix("unshallow ") {
                            info.unshallow.push(ObjectId::from_hex(self.algorithm, h)?);
                        } else {
                            return Err(Error::Protocol(format!(
                                "unexpected shallow line {line:?}"
                            )));
                        }
                    }
                    Packet::Flush => break,
                    other => {
                        return Err(Error::Protocol(format!(
                            "unexpected {other:?} in shallow lines"
                        )));
                    }
                }
            }
            self.shallow_info = info;
        }
        loop {
            match pkts.read()? {
                Packet::Data(d) if d.starts_with(b"ACK ") || d.starts_with(b"NAK") => continue,
                first => return Ok(SidebandReader::with_first(pkts, first, Some(progress))?),
            }
        }
    }
}

/// Reads response sections up to and including the `packfile` header. Returns whether a packfile follows.
fn read_fetch_sections<R: Read>(
    pkts: &mut PktReader<R>,
    algorithm: Algorithm,
    acks: &mut Vec<ObjectId>,
    info: &mut ShallowInfo,
) -> Result<bool> {
    loop {
        let header = match pkts.read()? {
            Packet::Data(d) => d,
            Packet::Flush => return Ok(false),
            other => {
                return Err(Error::Protocol(format!(
                    "unexpected {other:?} where a section header was expected"
                )));
            }
        };
        match text(&header)? {
            "acknowledgments" => loop {
                match pkts.read()? {
                    Packet::Data(d) => {
                        let line = text(&d)?;
                        if let Some(hex) = line.strip_prefix("ACK ") {
                            acks.push(ObjectId::from_hex(algorithm, hex)?);
                        } else if line != "NAK" && line != "ready" {
                            return Err(Error::Protocol(format!(
                                "unexpected acknowledgment {line:?}"
                            )));
                        }
                    }
                    Packet::Delim => break,
                    Packet::Flush => return Ok(false),
                    other => {
                        return Err(Error::Protocol(format!(
                            "unexpected {other:?} in acknowledgments"
                        )));
                    }
                }
            },
            "packfile" => return Ok(true),
            "shallow-info" => loop {
                match pkts.read()? {
                    Packet::Data(d) => {
                        let line = text(&d)?;
                        if let Some(h) = line.strip_prefix("shallow ") {
                            info.shallow.push(ObjectId::from_hex(algorithm, h)?);
                        } else if let Some(h) = line.strip_prefix("unshallow ") {
                            info.unshallow.push(ObjectId::from_hex(algorithm, h)?);
                        } else {
                            return Err(Error::Protocol(format!(
                                "unexpected shallow-info line {line:?}"
                            )));
                        }
                    }
                    Packet::Delim => break,
                    other => {
                        return Err(Error::Protocol(format!(
                            "unexpected {other:?} in shallow-info"
                        )));
                    }
                }
            },
            "wanted-refs" | "packfile-uris" => loop {
                match pkts.read()? {
                    Packet::Data(_) => {}
                    Packet::Delim => break,
                    other => {
                        return Err(Error::Protocol(format!(
                            "unexpected {other:?} in a skipped section"
                        )));
                    }
                }
            },
            other => {
                return Err(Error::Protocol(format!(
                    "unknown response section {other:?}"
                )));
            }
        }
    }
}

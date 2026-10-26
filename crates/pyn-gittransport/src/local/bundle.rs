//! A bundle file served as a protocol v2 upload-pack: its refs, and its pack as the answer to a fetch.

use std::collections::HashSet;
use std::io::{BufWriter, Read};
use std::path::{Path, PathBuf};

use pyn_git::Bundle;
use pyn_gitwire::pkt::PktWriter;
use pyn_gitwire::{Error as Wire, Session};
use pyn_object::ObjectId;

use super::upload::{finish, read_request};
use super::{BandWriter, parse_id, scratch};
use crate::Result;

pub(super) fn open(path: &Path) -> pyn_gitwire::Result<Box<dyn Session>> {
    let bundle = Bundle::open(path).map_err(|e| {
        Wire::Connection(format!(
            "'{}' does not look like a v2 or v3 bundle file: {e}",
            path.display()
        ))
    })?;
    if bundle.filter.is_some() {
        return Err(Wire::Unsupported("a bundle with a filter".into()));
    }
    let mut w = PktWriter::new(Vec::new());
    w.write_line("version 2")?;
    w.write_line(pyn_gitwire::caps::AGENT)?;
    w.write_line("ls-refs")?;
    w.write_line("fetch")?;
    w.write_line(&format!("object-format={}", bundle.algorithm.name()))?;
    w.write_flush()?;
    Ok(Box::new(BundleSession {
        path: path.to_owned(),
        bundle,
        advertisement: Some(w.into_inner()),
        reply: Box::new(std::io::empty()),
    }))
}

struct BundleSession {
    path: PathBuf,
    bundle: Bundle,
    advertisement: Option<Vec<u8>>,
    reply: Box<dyn Read>,
}

impl Session for BundleSession {
    fn advertisement(&mut self) -> pyn_gitwire::Result<Box<dyn Read + '_>> {
        let bytes = self.advertisement.take().unwrap_or_default();
        Ok(Box::new(std::io::Cursor::new(bytes)))
    }

    fn send(&mut self, body: &mut dyn Read) -> pyn_gitwire::Result<()> {
        let req = read_request(body).map_err(into_wire)?;
        self.reply = match req.command.as_str() {
            "ls-refs" => self.ls_refs(&req.args),
            "fetch" => self.fetch(&req.args),
            other => Err(Wire::Remote(format!("unknown command '{other}'")).into()),
        }
        .map_err(into_wire)?;
        Ok(())
    }

    fn response(&mut self) -> pyn_gitwire::Result<Box<dyn Read + '_>> {
        Ok(Box::new(&mut *self.reply))
    }
}

fn into_wire(e: crate::Error) -> Wire {
    match e {
        crate::Error::Wire(w) => w,
        other => Wire::Remote(other.to_string()),
    }
}

impl BundleSession {
    fn ls_refs(&self, args: &[String]) -> Result<Box<dyn Read>> {
        let mut prefixes = Vec::new();
        for a in args {
            match a.as_str() {
                "peel" | "symrefs" | "unborn" => {}
                other => match other.strip_prefix("ref-prefix ") {
                    Some(p) => prefixes.push(p),
                    None => {
                        return Err(Wire::Remote(format!("unexpected line: '{other}'")).into());
                    }
                },
            }
        }
        let mut w = PktWriter::new(Vec::new());
        for (name, id) in &self.bundle.refs {
            if prefixes.is_empty() || prefixes.iter().any(|p| name.starts_with(p)) {
                w.write_line(&format!("{id} {name}"))?;
            }
        }
        w.write_flush()?;
        Ok(Box::new(std::io::Cursor::new(w.into_inner())))
    }

    fn fetch(&self, args: &[String]) -> Result<Box<dyn Read>> {
        let alg = self.bundle.algorithm;
        let (mut wants, mut haves, mut done) = (Vec::new(), HashSet::new(), false);
        for a in args {
            match a.as_str() {
                "done" => done = true,
                "thin-pack" | "ofs-delta" | "no-progress" | "include-tag" => {}
                other => {
                    if let Some(h) = other.strip_prefix("want ") {
                        wants.push(parse_id(alg, h)?);
                    } else if let Some(h) = other.strip_prefix("have ") {
                        haves.insert(parse_id(alg, h)?);
                    } else if other.starts_with("shallow ") || other.starts_with("deepen") {
                        return Err(Wire::Remote("a bundle does not support shallow".into()).into());
                    } else {
                        return Err(Wire::Remote(format!("unexpected line: '{other}'")).into());
                    }
                }
            }
        }
        let held: HashSet<ObjectId> = self.bundle.refs.iter().map(|(_, id)| *id).collect();
        if let Some(w) = wants.iter().find(|w| !held.contains(w)) {
            return Err(Wire::Remote(format!("upload-pack: not our ref {w}")).into());
        }
        let common: Vec<ObjectId> = self
            .bundle
            .prerequisites
            .iter()
            .map(|p| p.id)
            .filter(|id| haves.contains(id))
            .collect();
        let ready = common.len() == self.bundle.prerequisites.len();
        let mut out = BufWriter::new(scratch()?);
        {
            let mut w = PktWriter::new(&mut out);
            if !done {
                w.write_line("acknowledgments")?;
                if common.is_empty() {
                    w.write_line("NAK")?;
                }
                for id in &common {
                    w.write_line(&format!("ACK {id}"))?;
                }
                if !ready || common.is_empty() {
                    w.write_flush()?;
                    return finish(out);
                }
                w.write_line("ready")?;
                w.write_delim()?;
            }
            w.write_line("packfile")?;
        }
        let mut pack = self.bundle.pack(&self.path)?;
        let mut band = BandWriter::new(&mut out, 1);
        std::io::copy(&mut pack, &mut band)?;
        band.finish()?;
        PktWriter::new(&mut out).write_flush()?;
        finish(out)
    }
}

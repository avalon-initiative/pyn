//! Protocol v2 upload-pack: `ls-refs` and `fetch` answered from a repository.

use std::collections::HashSet;
use std::io::{BufWriter, Read, Write};

use pyn_git::{Config, Ref, Repository, peel, reachable_objects, write_pack};
use pyn_gitwire::Error as Wire;
use pyn_gitwire::pkt::{Packet, PktReader, PktWriter, text};

use crate::Result;
use pyn_object::ObjectId;

use super::{BandWriter, parse_id, refuse_hidden_refs, scratch};

const HIDDEN: [&str; 2] = ["uploadpack.hiderefs", "transfer.hiderefs"];

pub(super) fn advertisement(repo: &Repository, config: &Config) -> Result<Vec<u8>> {
    refuse_hidden_refs(config, &HIDDEN)?;
    let mut w = PktWriter::new(Vec::new());
    w.write_line("version 2")?;
    w.write_line(pyn_gitwire::caps::AGENT)?;
    w.write_line("ls-refs=unborn")?;
    w.write_line("fetch")?;
    w.write_line(&format!("object-format={}", repo.algorithm().name()))?;
    w.write_flush()?;
    Ok(w.into_inner())
}

struct Request {
    command: String,
    args: Vec<String>,
}

fn read_request(body: &mut dyn Read) -> Result<Request> {
    let mut pkts = PktReader::new(body);
    let mut command = None;
    loop {
        match pkts.read()? {
            Packet::Data(d) => {
                let line = text(&d)?;
                if let Some(c) = line.strip_prefix("command=") {
                    command = Some(c.to_owned());
                }
            }
            Packet::Delim => break,
            Packet::Flush => {
                return Ok(Request {
                    command: command.ok_or_else(|| Wire::Protocol("no command".into()))?,
                    args: Vec::new(),
                });
            }
            Packet::ResponseEnd => {
                return Err(Wire::Protocol("unexpected response-end".into()).into());
            }
        }
    }
    let command = command.ok_or_else(|| Wire::Protocol("no command".into()))?;
    let args = pkts
        .read_until_flush()?
        .iter()
        .map(|l| Ok(text(l).map(str::to_owned)?))
        .collect::<Result<_>>()?;
    Ok(Request { command, args })
}

pub(super) fn serve(
    repo: &Repository,
    _config: &Config,
    body: &mut dyn Read,
) -> Result<Box<dyn Read>> {
    let req = read_request(body)?;
    match req.command.as_str() {
        "ls-refs" => ls_refs(repo, &req.args),
        "fetch" => fetch(repo, &req.args),
        other => Err(Wire::Remote(format!("unknown command '{other}'")).into()),
    }
}

fn ls_refs(repo: &Repository, args: &[String]) -> Result<Box<dyn Read>> {
    let (mut peel_tags, mut symrefs, mut unborn) = (false, false, false);
    let mut prefixes = Vec::new();
    for a in args {
        match a.as_str() {
            "peel" => peel_tags = true,
            "symrefs" => symrefs = true,
            "unborn" => unborn = true,
            other => match other.strip_prefix("ref-prefix ") {
                Some(p) => prefixes.push(p),
                None => return Err(Wire::Remote(format!("unexpected line: '{other}'")).into()),
            },
        }
    }
    let wanted = |name: &str| prefixes.is_empty() || prefixes.iter().any(|p| name.starts_with(p));
    let refs = repo.refs();
    let db = repo.objects()?;
    let line = |name: &str, id: ObjectId, target: Option<&str>| {
        let mut l = format!("{id} {name}");
        if let Some(t) = target.filter(|_| symrefs) {
            l.push_str(&format!(" symref-target:{t}"));
        }
        if peel_tags
            && let Ok((tags, end, _)) = peel(&db, id)
            && !tags.is_empty()
        {
            l.push_str(&format!(" peeled:{end}"));
        }
        l
    };
    let mut lines = Vec::new();
    if wanted("HEAD") {
        match refs.head()? {
            Ref::Symbolic(target) => match refs.resolve("HEAD")? {
                Some(id) => lines.push(line("HEAD", id, Some(&target))),
                None if unborn => lines.push(if symrefs {
                    format!("unborn HEAD symref-target:{target}")
                } else {
                    "unborn HEAD".to_owned()
                }),
                None => {}
            },
            Ref::Direct(id) => lines.push(line("HEAD", id, None)),
        }
    }
    for (name, id) in refs.list("refs/")? {
        if wanted(&name) {
            lines.push(line(&name, id, None));
        }
    }
    let mut w = PktWriter::new(Vec::new());
    for l in lines {
        w.write_line(&l)?;
    }
    w.write_flush()?;
    Ok(Box::new(std::io::Cursor::new(w.into_inner())))
}

fn fetch(repo: &Repository, args: &[String]) -> Result<Box<dyn Read>> {
    let db = repo.objects()?;
    let alg = repo.algorithm();
    let (mut wants, mut haves, mut done) = (Vec::new(), Vec::new(), false);
    for a in args {
        match a.as_str() {
            "done" => done = true,
            "thin-pack" | "ofs-delta" | "no-progress" => {}
            other => {
                if let Some(h) = other.strip_prefix("want ") {
                    wants.push(parse_id(alg, h)?);
                } else if let Some(h) = other.strip_prefix("have ") {
                    haves.push(parse_id(alg, h)?);
                } else {
                    return Err(Wire::Remote(format!("unexpected line: '{other}'")).into());
                }
            }
        }
    }
    if let Some(missing) = wants.iter().find(|w| !db.contains(w).unwrap_or(false)) {
        return Err(Wire::Remote(format!("upload-pack: not our ref {missing}")).into());
    }
    let mut seen = HashSet::new();
    let common: Vec<ObjectId> = haves
        .into_iter()
        .filter(|h| seen.insert(*h) && db.contains(h).unwrap_or(false))
        .collect();
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
            if common.is_empty() {
                w.write_flush()?;
                return finish(out);
            }
            w.write_line("ready")?;
            w.write_delim()?;
        }
        w.write_line("packfile")?;
    }
    let objects = reachable_objects(&db, &wants, &common)?.objects;
    let mut band = BandWriter::new(&mut out, 1);
    write_pack(&db, &objects, &mut band)?;
    band.finish()?;
    PktWriter::new(&mut out).write_flush()?;
    finish(out)
}

fn finish(out: BufWriter<std::fs::File>) -> Result<Box<dyn Read>> {
    use std::io::{Seek, SeekFrom};
    let mut file = out.into_inner().map_err(|e| e.into_error())?;
    file.flush()?;
    file.seek(SeekFrom::Start(0))?;
    Ok(Box::new(std::io::BufReader::new(file)))
}

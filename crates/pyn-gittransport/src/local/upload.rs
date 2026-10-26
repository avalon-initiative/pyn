//! Protocol v2 upload-pack: `ls-refs` and `fetch` answered from a repository.

use std::collections::HashSet;
use std::io::{BufWriter, Read, Write};

use pyn_git::{
    Config, Deepen, ObjectFilter, Ref, Repository, peel, reachable_objects_filtered, real_parents,
    shallow_cut, write_pack,
};
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
    let filter = config.get_bool("uploadpack.allowfilter")?.unwrap_or(false);
    w.write_line(if filter {
        "fetch=shallow filter"
    } else {
        "fetch=shallow"
    })?;
    w.write_line(&format!("object-format={}", repo.algorithm().name()))?;
    w.write_flush()?;
    Ok(w.into_inner())
}

pub(super) struct Request {
    pub(super) command: String,
    pub(super) args: Vec<String>,
}

pub(super) fn read_request(body: &mut dyn Read) -> Result<Request> {
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
    let mut client_shallow = HashSet::new();
    let (mut depth, mut relative, mut since, mut not) = (None, false, None, Vec::new());
    let mut filter = None;
    for a in args {
        match a.as_str() {
            "done" => done = true,
            "thin-pack" | "ofs-delta" | "no-progress" => {}
            "deepen-relative" => relative = true,
            other => {
                if let Some(h) = other.strip_prefix("shallow ") {
                    client_shallow.insert(parse_id(alg, h)?);
                } else if let Some(n) = other.strip_prefix("deepen ") {
                    depth = Some(
                        n.parse::<u32>()
                            .map_err(|_| Wire::Remote(format!("expected a depth, got '{n}'")))?,
                    );
                } else if let Some(t) = other.strip_prefix("deepen-since ") {
                    since = Some(
                        t.parse::<i64>()
                            .map_err(|_| Wire::Remote(format!("invalid deepen-since '{t}'")))?,
                    );
                } else if let Some(r) = other.strip_prefix("deepen-not ") {
                    not.push(resolve_ref(repo, r)?);
                } else if let Some(spec) = other.strip_prefix("filter ") {
                    filter = Some(
                        ObjectFilter::parse(spec)
                            .map_err(|_| Wire::Remote(format!("invalid filter-spec '{spec}'")))?,
                    );
                } else if let Some(h) = other.strip_prefix("want ") {
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
    client_shallow.retain(|c| db.contains(c).unwrap_or(false));
    let deepen = match (depth, since, not.is_empty()) {
        (Some(n), None, true) => Some(Deepen::Depth { n, relative }),
        (None, Some(t), true) => Some(Deepen::Since(t)),
        (None, None, false) => Some(Deepen::Not(not)),
        (None, None, true) => None,
        _ => {
            return Err(Wire::Remote(
                "deepen and deepen-since (or deepen-not) cannot be used together".into(),
            )
            .into());
        }
    };
    let mut seen = HashSet::new();
    let common: Vec<ObjectId> = haves
        .into_iter()
        .filter(|h| seen.insert(*h) && db.contains(h).unwrap_or(false))
        .collect();
    let cut = match &deepen {
        Some(d) => {
            shallow_cut(&db, &wants, &client_shallow, d).map_err(|e| Wire::Remote(e.to_string()))?
        }
        None => pyn_git::ShallowCut::default(),
    };
    let mut pack_wants = wants.clone();
    for s in &cut.unshallow {
        pack_wants.extend(real_parents(&db, s)?);
    }
    let own = db_shallow(repo)?;
    let mut grafts = client_shallow.clone();
    grafts.extend(cut.shallow.iter().copied());
    if !grafts.is_empty() {
        let mut all = own.clone();
        all.extend(grafts);
        db.set_shallow(all);
    }
    let objects = if done || !common.is_empty() {
        reachable_objects_filtered(&db, &pack_wants, &common, filter.as_ref())?.objects
    } else {
        Vec::new()
    };
    let inherited: Vec<ObjectId> = objects
        .iter()
        .filter(|o| own.contains(o) && !client_shallow.contains(o) && !cut.shallow.contains(o))
        .copied()
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
        if deepen.is_some() || !client_shallow.is_empty() || !inherited.is_empty() {
            w.write_line("shallow-info")?;
            for id in cut.shallow.iter().chain(&inherited) {
                w.write_line(&format!("shallow {id}"))?;
            }
            for id in &cut.unshallow {
                w.write_line(&format!("unshallow {id}"))?;
            }
            w.write_delim()?;
        }
        w.write_line("packfile")?;
    }
    let mut band = BandWriter::new(&mut out, 1);
    write_pack(&db, &objects, &mut band)?;
    band.finish()?;
    PktWriter::new(&mut out).write_flush()?;
    finish(out)
}

fn db_shallow(repo: &Repository) -> Result<HashSet<ObjectId>> {
    Ok(pyn_git::read_shallow(repo.common_dir(), repo.algorithm())?)
}

fn resolve_ref(repo: &Repository, name: &str) -> Result<ObjectId> {
    let refs = repo.refs();
    for candidate in [
        name.to_owned(),
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
    ] {
        if let Some(id) = refs.resolve(&candidate)? {
            return Ok(id);
        }
    }
    Err(Wire::Remote(format!("git upload-pack: deepen-not is not a ref: {name}")).into())
}

pub(super) fn finish(out: BufWriter<std::fs::File>) -> Result<Box<dyn Read>> {
    use std::io::{Seek, SeekFrom};
    let mut file = out.into_inner().map_err(|e| e.into_error())?;
    file.flush()?;
    file.seek(SeekFrom::Start(0))?;
    Ok(Box::new(std::io::BufReader::new(file)))
}

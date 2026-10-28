//! Moving objects between a repository and a remote: fetch into the object database, push from it.

use std::collections::HashSet;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use pyn_git::{
    IngestLimits, IngestedPack, ObjectDb, Repository, index_pack, peel, reachable_objects,
    read_shallow, real_parents, write_shallow,
};
use pyn_gitwire::{
    Deepen, FetchRequest, HaveSource, LsRefsOptions, NoHaves, PushOptions, PushReport, ReceivePack,
    RefUpdate, RemoteRef, Transport, UploadPack,
};
use pyn_object::{Algorithm, Kind, ObjectId};

use crate::error::Result;

/// Receives the server's progress text (sideband channel 2, which includes hook output on a push).
pub type Progress<'a> = &'a mut dyn FnMut(&[u8]);

const HAVE_WINDOW: usize = 8192;

/// Commits the repository has, newest first, from its branches, remote-tracking branches, tags and `HEAD`.
struct LocalHaves<'a> {
    db: &'a ObjectDb,
    tips: Vec<ObjectId>,
    ordered: Vec<ObjectId>,
    given: usize,
    window: usize,
    complete: bool,
}

impl<'a> LocalHaves<'a> {
    fn new(repo: &Repository, db: &'a ObjectDb) -> Result<Self> {
        let refs = repo.refs();
        let mut tips = Vec::new();
        for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
            tips.extend(refs.list(prefix)?.into_iter().map(|(_, id)| id));
        }
        tips.extend(refs.resolve("HEAD")?);
        let mut seen = std::collections::HashSet::new();
        tips.retain(|t| seen.insert(*t));
        Ok(Self {
            tips: pyn_git::existing(db, &tips)?,
            db,
            ordered: Vec::new(),
            given: 0,
            window: HAVE_WINDOW,
            complete: false,
        })
    }

    fn fill(&mut self, wanted: usize) {
        while !self.complete && self.ordered.len() < wanted {
            let window = self.window.max(wanted);
            let all =
                pyn_git::commits_newest_first(self.db, &self.tips, window).unwrap_or_default();
            self.complete = all.len() < window;
            self.ordered = all;
            self.window = window * 4;
        }
    }
}

impl HaveSource for LocalHaves<'_> {
    fn next_batch(&mut self, max: usize) -> Vec<ObjectId> {
        self.fill(self.given + max);
        let end = (self.given + max).min(self.ordered.len());
        let batch = self.ordered[self.given..end].to_vec();
        self.given = end;
        batch
    }
}

/// The refs a server holds. Detects the object format when `algorithm` is `None` (a repository-less call).
pub fn list_remote_refs(
    transport: &mut dyn Transport,
    algorithm: Option<Algorithm>,
    opts: &LsRefsOptions,
) -> Result<(Algorithm, Vec<RemoteRef>)> {
    let mut alg = algorithm.unwrap_or(Algorithm::Sha1);
    let mut up = match UploadPack::open(transport, alg) {
        Err(pyn_gitwire::Error::ObjectFormatMismatch { server, .. }) if algorithm.is_none() => {
            alg = server;
            UploadPack::open(transport, alg)?
        }
        other => other?,
    };
    let refs = up.ls_refs(opts)?;
    Ok((alg, refs))
}

/// Fetches what `wants` names over an open session. Returns `None` when the repository already has every want.
pub fn fetch_with(
    up: &mut UploadPack,
    repo: &Repository,
    wants: &[ObjectId],
    progress: Progress<'_>,
) -> Result<Option<IngestedPack>> {
    fetch_shallow_with(up, repo, wants, None, progress)
}

/// Like [`fetch_with`], limiting history by `deepen` and recording the new shallow boundary in `shallow`.
/// A shallow request goes to the server even when every want is held, since it may only move the boundary.
pub fn fetch_shallow_with(
    up: &mut UploadPack,
    repo: &Repository,
    wants: &[ObjectId],
    deepen: Option<Deepen>,
    progress: Progress<'_>,
) -> Result<Option<IngestedPack>> {
    let opts = Partial {
        deepen,
        ..Partial::default()
    };
    Ok(fetch_partial_with(up, repo, wants, &opts, progress)?.pack)
}

/// What a fetch asks beyond the wants.
#[derive(Debug, Clone, Default)]
pub struct Partial {
    pub deepen: Option<Deepen>,
    /// A filter spec the server applies (`blob:none`).
    pub filter: Option<String>,
    /// The remote is a promisor remote: the pack received is marked so what it leaves out counts as promised.
    pub promisor: bool,
    /// Ask for every object as a fresh clone would, without negotiating.
    pub refetch: bool,
    /// Record the shallow roots a shallow server announces; otherwise they are only reported in `held_back`.
    pub update_shallow: bool,
}

/// What a partial fetch brought.
#[derive(Debug)]
pub struct Fetched {
    pub pack: Option<IngestedPack>,
    /// New shallow roots of the remote that were not recorded (no `update_shallow`).
    pub held_back: HashSet<ObjectId>,
}

/// Like [`fetch_shallow_with`], with the partial-clone options of `opts`.
pub fn fetch_partial_with(
    up: &mut UploadPack,
    repo: &Repository,
    wants: &[ObjectId],
    opts: &Partial,
    progress: Progress<'_>,
) -> Result<Fetched> {
    let db = repo.objects()?;
    let mut held_back = HashSet::new();
    let held = read_shallow(repo.common_dir(), repo.algorithm())?;
    let tips: HashSet<ObjectId> = repo
        .refs()
        .list("refs/")?
        .into_iter()
        .map(|(_, i)| i)
        .collect();
    let missing = if opts.deepen.is_some() || opts.refetch {
        wants.to_vec()
    } else {
        wants
            .iter()
            .filter(|w| !db.contains(w).unwrap_or(false) || has_gap(&db, &held, &tips, w))
            .copied()
            .collect::<Vec<_>>()
    };
    if missing.is_empty() {
        return Ok(Fetched {
            pack: None,
            held_back,
        });
    }
    let mut req = FetchRequest::new(missing);
    req.thin_pack = !opts.refetch && opts.filter.is_none() && !opts.promisor;
    req.ofs_delta = true;
    req.shallow = held.iter().copied().collect();
    req.shallow.sort_by_key(ObjectId::to_hex);
    req.deepen = opts.deepen.clone();
    req.filter = opts.filter.clone();
    let ingested = if opts.refetch {
        fetch_pack_into(up, repo, &req, &mut NoHaves, opts.promisor, progress)?
    } else {
        let mut haves = LocalHaves::new(repo, &db)?;
        fetch_pack_into(up, repo, &req, &mut haves, opts.promisor, progress)?
    };
    let info = up.shallow_info();
    let (mut roots, mut unshallow) = (info.shallow.clone(), info.unshallow.clone());
    if opts.deepen.is_none() {
        roots.retain(|id| !held.contains(id) && lacks_parents(&db, &held, id));
        unshallow.clear();
        if !opts.update_shallow {
            held_back.extend(roots.drain(..));
        }
    }
    if !roots.is_empty() || !unshallow.is_empty() {
        let mut now = held;
        for id in &unshallow {
            now.remove(id);
        }
        now.extend(roots);
        write_shallow(repo.common_dir(), &now)?;
    }
    Ok(Fetched {
        pack: Some(ingested),
        held_back,
    })
}

/// Whether `id` is a commit, not a shallow boundary, with a parent this repository lacks.
fn lacks_parents(db: &ObjectDb, held: &HashSet<ObjectId>, id: &ObjectId) -> bool {
    !held.contains(id)
        && real_parents(db, id).is_ok_and(|ps| ps.iter().any(|p| !db.contains(p).unwrap_or(true)))
}

/// Whether the history of `want` has a gap before it meets `tips`, the way an unrecorded shallow root leaves one.
fn has_gap(
    db: &ObjectDb,
    held: &HashSet<ObjectId>,
    tips: &HashSet<ObjectId>,
    want: &ObjectId,
) -> bool {
    let Ok((_, start, Kind::Commit)) = peel(db, *want) else {
        return false;
    };
    let mut seen = HashSet::from([start]);
    let mut todo = vec![start];
    while let Some(c) = todo.pop() {
        if tips.contains(&c) || held.contains(&c) {
            continue;
        }
        let Ok(parents) = real_parents(db, &c) else {
            return false;
        };
        for p in parents {
            if !db.contains(&p).unwrap_or(true) {
                return true;
            }
            if seen.insert(p) {
                todo.push(p);
            }
        }
    }
    false
}

/// Sends `req` and stores the pack that comes back.
pub fn fetch_pack_into(
    up: &mut UploadPack,
    repo: &Repository,
    req: &FetchRequest,
    haves: &mut dyn HaveSource,
    promisor: bool,
    progress: Progress<'_>,
) -> Result<IngestedPack> {
    let db = repo.objects()?;
    let mut pack = up.fetch(req, haves, Box::new(|m| progress(m)))?;
    let ingested = index_pack(&db, &mut pack, &IngestLimits::from_config(repo.config())?)?;
    std::io::copy(&mut pack, &mut std::io::sink())?;
    drop(pack);
    if promisor {
        ingested.mark_promisor()?;
    }
    Ok(ingested)
}

/// Opens an upload-pack session on `transport` and fetches `wants` into `repo`'s object database.
pub fn fetch_objects(
    repo: &Repository,
    transport: &mut dyn Transport,
    wants: &[ObjectId],
    progress: Progress<'_>,
) -> Result<Option<IngestedPack>> {
    let mut up = UploadPack::open(transport, repo.algorithm())?;
    fetch_with(&mut up, repo, wants, progress)
}

/// Sends the objects the server lacks for `updates` over an open session, then the ref commands.
pub fn push_with(
    rp: &mut ReceivePack,
    repo: &Repository,
    updates: &[RefUpdate],
    opts: &PushOptions,
    progress: Progress<'_>,
) -> Result<PushReport> {
    let db = repo.objects()?;
    let want: Vec<ObjectId> = updates.iter().filter_map(|u| u.new).collect();
    if want.is_empty() {
        return Ok(rp.push(updates, None, opts, progress)?);
    }
    let advertised: Vec<ObjectId> = rp.refs().iter().map(|r| r.id).collect();
    let have = pyn_git::existing(&db, &advertised)?;
    let reachable = reachable_objects(&db, &want, &have)?;
    let mut file = tempfile::tempfile_in(db.dir())?;
    {
        let mut out = BufWriter::new(&mut file);
        pyn_git::write_pack(&db, &reachable.objects, &mut out)?;
        out.flush()?;
    }
    file.seek(SeekFrom::Start(0))?;
    let mut pack = BufReader::new(file);
    let pack: &mut dyn Read = &mut pack;
    Ok(rp.push(updates, Some(pack), opts, progress)?)
}

/// Opens a receive-pack session on `transport` and pushes `updates` from `repo`.
pub fn push_objects(
    repo: &Repository,
    transport: &mut dyn Transport,
    updates: &[RefUpdate],
    opts: &PushOptions,
    progress: Progress<'_>,
) -> Result<PushReport> {
    let mut rp = ReceivePack::open(transport, repo.algorithm())?;
    push_with(&mut rp, repo, updates, opts, progress)
}

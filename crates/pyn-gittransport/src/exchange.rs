//! Moving objects between a repository and a remote: fetch into the object database, push from it.

use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use pyn_git::{IngestLimits, IngestedPack, ObjectDb, Repository, index_pack, reachable_objects};
use pyn_gitwire::{
    FetchRequest, HaveSource, LsRefsOptions, PushOptions, PushReport, ReceivePack, RefUpdate,
    RemoteRef, Transport, UploadPack,
};
use pyn_object::{Algorithm, ObjectId};

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
    let db = repo.objects()?;
    let missing = wants
        .iter()
        .filter(|w| !db.contains(w).unwrap_or(false))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(None);
    }
    let mut req = FetchRequest::new(missing);
    req.thin_pack = true;
    req.ofs_delta = true;
    let mut haves = LocalHaves::new(repo, &db)?;
    let mut pack = up.fetch(&req, &mut haves, Box::new(|m| progress(m)))?;
    let ingested = index_pack(&db, &mut pack, &IngestLimits::default())?;
    std::io::copy(&mut pack, &mut std::io::sink())?;
    Ok(Some(ingested))
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
